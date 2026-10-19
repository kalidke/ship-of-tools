#!/usr/bin/env bash
# installer-shared-home.sh -- a home several hosts share holds one install: every path that writes, re-renders or
# retires sotd.service pins it to the hosts that run sotd (`sotd topology pin`) before any reload, and no retire
# disables the shared unit, under a declared topology or, with no hosts.toml, on a host its pin does not name alone.
# The functions and install.sh run for real; `sotd`, `systemctl`, `loginctl`, `curl` and `git` are stubs, HOME is
# scratch, and install.sh's PATH is the stubs and `INSTALL_TOOLS`, never the host's /usr/bin: a Julia or juliaup there
# would skip the Julia install the network case reads. Run: scripts/tests/installer-shared-home.sh

set -euo pipefail

# shellcheck source=installer-support.sh
. "$(dirname "$0")/installer-support.sh"

TEMPLATE="$(dirname "$0")/../../deploy/sotd.service"

# A scratch home and prefix whose sotd, systemctl and loginctl append their argv to one log, in call order.
pin_fixture() {  # <dir>
    local d="$1"
    mkdir -p "$d/home/.config/systemd/user" "$d/prefix/bin" "$d/stubs"
    : > "$d/log"
    printf '#!/bin/sh\nprintf "sotd %%s\\n" "$*" >> "%s/log"\n' "$d" > "$d/prefix/bin/sotd"
    cat > "$d/stubs/systemctl" <<STUBEOF
#!/bin/sh
printf 'systemctl %s\n' "\$*" >> "$d/log"
STUBEOF
    printf '#!/bin/sh\nprintf "loginctl %%s\\n" "$*" >> "%s/log"\n' "$d" > "$d/stubs/loginctl"
    chmod +x "$d/prefix/bin/sotd" "$d/stubs/systemctl" "$d/stubs/loginctl"
}
# install.sh's tools: the library's, the archive tools it unpacks a release with, and jq, which its preflight requires.
INSTALL_TOOLS="$WORK/install-tools"; mk_tools "$INSTALL_TOOLS" tar gzip tr jq
# The line number of the first log line equal to <line>, or 0.
at() { grep -n -x -F -- "$2" "$1/log" | head -1 | cut -d: -f1 || true; }
PIN_LINE() { printf 'sotd topology pin --dir %s/home/.config/systemd/user' "$1"; }

# ---------------------------------------------------------------------------
case_start "install_pins_the_unit_before_it_is_loaded"
d="$WORK/enable"; pin_fixture "$d"
( HOME="$d/home"; PATH="$d/stubs:$PATH"; installer_enable_local_service "$d/prefix" "$TEMPLATE" "$d/sot.sock" ) >/dev/null 2>&1 || true
pin="$(at "$d" "$(PIN_LINE "$d") --claim")"; reload="$(at "$d" 'systemctl --user daemon-reload')"; start="$(at "$d" 'systemctl --user start sotd.service')"
check "the unit is written" "yes" "$([ -f "$d/home/.config/systemd/user/sotd.service" ] && echo yes || echo no)"
check "the pin is written into the unit's folder, claimed for this host when there is no hosts.toml" "yes" "$([ "${pin:-0}" -gt 0 ] && echo yes || echo no)"
check "pin, then reload, then start" "yes" "$([ "${pin:-0}" -gt 0 ] && [ "$pin" -lt "${reload:-0}" ] && [ "$reload" -lt "${start:-0}" ] && echo yes || echo no)"
check "the boot drop-in is written, and no enable is run" "yes 0" "$([ -f "$d/home/.config/systemd/user/default.target.d/sotd.conf" ] && echo yes || echo no) $(grep -c 'enable --now\|--user enable' "$d/log" || true)"

# ---------------------------------------------------------------------------
case_start "install_wires_boot_start_by_one_rule_for_both_update_paths"
# One rule: a run that finds no sotd.service wires boot start; a run over a unit wires it only where an old link or the
# drop-in is, so an install.sh run over a unit a person turned off keeps it off, as an update through sot-apply does.
wire_run() {  # <name> <state: fresh|link|dropin|none>
    local dd="$WORK/wire-$1" uu="$WORK/wire-$1/home/.config/systemd/user"
    pin_fixture "$dd"
    if [ "$2" != fresh ]; then
        printf '[Service]\nExecStart=%s/bin/sotd --x\n' "$dd/prefix" > "$uu/sotd.service"
        case "$2" in
            link) mkdir -p "$uu/default.target.wants"; ln -s ../sotd.service "$uu/default.target.wants/sotd.service" ;;
            dropin) mkdir -p "$uu/default.target.d"; printf '[Unit]\nWants=sotd.service\n' > "$uu/default.target.d/sotd.conf" ;;
        esac
    fi
    ( HOME="$dd/home"; PATH="$dd/stubs:$PATH"; installer_enable_local_service "$dd/prefix" "$TEMPLATE" "$dd/sot.sock" ) >/dev/null 2>&1 || true
    printf '%s %s' "$([ -f "$uu/default.target.d/sotd.conf" ] && echo dropin || echo none)" "$([ -L "$uu/default.target.wants/sotd.service" ] && echo link || echo none)"
}
check "a fresh install wires boot start" "dropin none" "$(wire_run fresh fresh)"
check "an install over an enabled unit moves its link onto the drop-in" "dropin none" "$(wire_run link link)"
check "an install over a wired unit keeps the drop-in" "dropin none" "$(wire_run dropin dropin)"
check "an install over a unit with neither writes nothing" "none none" "$(wire_run none none)"

# ---------------------------------------------------------------------------
case_start "retire_under_a_topology_pins_and_stops_and_never_disables"
d="$WORK/retire-topo"; pin_fixture "$d"
printf '[Service]\nExecStart=/x/bin/sotd\n' > "$d/home/.config/systemd/user/sotd.service"
( HOME="$d/home"; PATH="$d/stubs:$PATH"; installer_retire_local_service 0 "$d/prefix" 1 ) >/dev/null 2>&1 || true
pin="$(at "$d" "$(PIN_LINE "$d")")"; stop="$(at "$d" 'systemctl --user stop sotd.service')"; reload="$(at "$d" 'systemctl --user daemon-reload')"
check "the shared unit is never disabled" "0" "$(grep -c 'disable' "$d/log" || true)"
check "pin, then stop this host's run, then reload" "yes" "$([ "${pin:-0}" -gt 0 ] && [ "$pin" -lt "${stop:-0}" ] && [ "$stop" -lt "${reload:-0}" ] && echo yes || echo no)"
check "the unit file stays" "yes" "$([ -f "$d/home/.config/systemd/user/sotd.service" ] && echo yes || echo no)"

d="$WORK/retire-topo-nounit"; pin_fixture "$d"
( HOME="$d/home"; PATH="$d/stubs:$PATH"; installer_retire_local_service 0 "$d/prefix" 1 ) >/dev/null 2>&1 || true
check "with no unit file nothing runs" "0" "$(wc -l < "$d/log" | tr -d ' ')"

# ---------------------------------------------------------------------------
case_start "a_home_two_hosts_share_with_no_hosts_toml"
# Host A installs a daemon, then host B, which shares the home, retires one. The hostname stand-in names the host each run
# is on; the boot start is the drop-in in the shared folder.
# unit_pin.rs proves that the real `sotd topology pin --claim` writes the pin installer_pin_names_only reads.
d="$WORK/two-hosts"; pin_fixture "$d"
u="$d/home/.config/systemd/user"; dropin="$u/default.target.d/sotd.conf"
cat > "$d/stubs/systemctl" <<STUBEOF
#!/bin/sh
printf '%s systemctl %s\n' "\$STUB_HOST" "\$*" >> "$d/log"
STUBEOF
printf '#!/bin/sh\nprintf "%%s\\n" "$STUB_HOST"\n' > "$d/stubs/hostname"
chmod +x "$d/stubs/systemctl" "$d/stubs/hostname"
on_host() {  # <hostname> <function> [args...]: one installer function as that host runs it
    ( HOME="$d/home"; PATH="$d/stubs:$PATH"; STUB_HOST="$1"; export STUB_HOST; shift; "$@" ) >/dev/null 2>&1 || true
}
linked() { [ -f "$dropin" ] && echo yes || echo no; }
on_host unithost-a.example.org installer_enable_local_service "$d/prefix" "$TEMPLATE" "$d/sot.sock"
check "host A's install claims the pin for itself" "1" "$(grep -c -x -F "$(PIN_LINE "$d") --claim" "$d/log" || true)"
check "host A's install wires the unit to start at boot" "yes" "$(linked)"
mkdir -p "$u/sotd.service.d"; printf '[Unit]\nConditionHost=|unithost-a\nConditionHost=|unithost-a.*\n' > "$u/sotd.service.d/topology.conf"
on_host unithost-b installer_retire_local_service 0 "$d/prefix" 0
check "host B's retire never disables the shared unit" "0" "$(grep -c '^unithost-b systemctl .*disable' "$d/log" || true)"
check "host B's retire stops its own run" "1" "$(grep -c -x -F 'unithost-b systemctl --user stop sotd.service' "$d/log" || true)"
check "host A's boot drop-in stays" "yes" "$(linked)"
on_host unithost-a.example.org installer_retire_local_service 0 "$d/prefix" 0
check "host A's own retire removes the drop-in its pin serves on host A alone" "no" "$(linked)"
on_host unithost-a.example.org sot_boot_wants "$u"
printf '[Unit]\nConditionHost=|unithost-a\nConditionHost=|unithost-a.*\nConditionHost=|unithost-b\nConditionHost=|unithost-b.*\n' \
    > "$u/sotd.service.d/topology.conf"
on_host unithost-a.example.org installer_retire_local_service 0 "$d/prefix" 0
check "a pin that names another host too keeps the drop-in" "yes" "$(linked)"
rm -f "$u/sotd.service.d/topology.conf"
on_host unithost-a.example.org installer_retire_local_service 0 "$d/prefix" 0
check "with no pin (an install from before pins) the drop-in stays" "yes" "$(linked)"

# The installer says what a pin move and a pre-pin retire did. The sotd stand-in writes the claim's pin for the host it runs on.
printf '#!/bin/sh\nh="$(hostname)"; printf "[Unit]\\nConditionHost=|%%s\\nConditionHost=|%%s.*\\n" "$h" "$h" > "%s/sotd.service.d/topology.conf"\n' "$u" > "$d/prefix/bin/sotd"
chmod +x "$d/prefix/bin/sotd"
rm -f "$u/sotd.service.d/topology.conf"
said() { ( HOME="$d/home"; PATH="$d/stubs:$PATH"; STUB_HOST="$1"; export STUB_HOST; shift; "$@" ) 2>&1 || true; }
said unithost-a installer_enable_local_service "$d/prefix" "$TEMPLATE" "$d/sot.sock" > "$WORK/say-a"
check "a first claim names no earlier host" "0" "$(grep -c 'moved here' "$WORK/say-a" || true)"
said unithost-b installer_enable_local_service "$d/prefix" "$TEMPLATE" "$d/sot.sock" > "$WORK/say-b"
check "host B's install names host A and that its sotd will not start" "1" "$(grep -c -F "The daemon moved here from unithost-a: unithost-a's sotd will not start at its next login." "$WORK/say-b" || true)"
said unithost-b installer_enable_local_service "$d/prefix" "$TEMPLATE" "$d/sot.sock" > "$WORK/say-b2"
check "a reinstall on the host that holds the pin says nothing of a move" "0" "$(grep -c 'moved here' "$WORK/say-b2" || true)"
rm -f "$u/sotd.service.d/topology.conf"
said unithost-a installer_retire_local_service 0 "$d/prefix" 0 > "$WORK/say-pre"
check "a pre-pin retire says it stopped, that enable and disable change no file, and what moves boot start" "1 1 1 1" \
    "$(grep -c -F "Stopped this computer's sotd. Boot start follows hosts.toml and the unit's host pin, not enable or disable, which change no file for sotd.service" "$WORK/say-pre" || true) $(grep -c -F 'sotd topology pin --dir ~/.config/systemd/user --claim' "$WORK/say-pre" || true) $(grep -c -F 'rm -f ~/.config/systemd/user/default.target.d/sotd.conf' "$WORK/say-pre" || true) $(grep -c -F 'write that file again' "$WORK/say-pre" || true)"
check "a pre-pin retire leaves the drop-in" "yes" "$(linked)"

# An install from before 0.6.6 left the enable link. A retire that does not own the pin alone moves it to the drop-in, so a
# hand disable cannot turn boot start off there either; one that owns the pin alone removes it; a drop-in stays.
oldlink() {  # <state: link|dropin|none>: the unit folder as an earlier install left it
    rm -rf "$u/default.target.wants" "$u/default.target.d"
    case "$1" in
        link) mkdir -p "$u/default.target.wants"; ln -s ../sotd.service "$u/default.target.wants/sotd.service" ;;
        dropin) mkdir -p "$u/default.target.d"; printf '[Unit]\nWants=sotd.service\n' > "$u/default.target.d/sotd.conf" ;;
    esac
}
wiring() { printf '%s %s' "$([ -f "$u/default.target.d/sotd.conf" ] && echo dropin || echo none)" "$([ -L "$u/default.target.wants/sotd.service" ] && echo link || echo none)"; }
oldlink none
said unithost-a installer_retire_local_service 0 "$d/prefix" 0 > "$WORK/say-off"
check "a retire over a unit with boot start off says it is already off, not that it follows hosts.toml, and writes nothing" "1 0 none none" \
    "$(grep -c -F 'Its boot start is already off' "$WORK/say-off" || true) $(grep -c -F 'Boot start follows hosts.toml' "$WORK/say-off" || true) $(wiring)"
oldlink link
on_host unithost-a installer_retire_local_service 0 "$d/prefix" 0
check "a pre-pin retire moves an old link onto the drop-in" "dropin none" "$(wiring)"
oldlink link
printf '[Unit]\nConditionHost=|unithost-a\nConditionHost=|unithost-a.*\nConditionHost=|unithost-b\nConditionHost=|unithost-b.*\n' > "$u/sotd.service.d/topology.conf"
on_host unithost-b installer_retire_local_service 0 "$d/prefix" 0
check "a retire whose pin names other hosts too moves an old link onto the drop-in" "dropin none" "$(wiring)"
oldlink link
on_host unithost-b installer_retire_local_service 0 "$d/prefix" 1
check "a retire under a declared topology moves an old link onto the drop-in" "dropin none" "$(wiring)"
oldlink dropin
on_host unithost-b installer_retire_local_service 0 "$d/prefix" 1
check "a retire under a declared topology keeps the drop-in the other hosts need" "dropin none" "$(wiring)"
oldlink none
on_host unithost-b installer_retire_local_service 0 "$d/prefix" 1
check "a retire under a declared topology over a unit with boot start off writes nothing" "none none" "$(wiring)"
printf '[Unit]\nConditionHost=|unithost-a\nConditionHost=|unithost-a.*\n' > "$u/sotd.service.d/topology.conf"
oldlink link
on_host unithost-a installer_retire_local_service 0 "$d/prefix" 0
check "a retire whose pin names this host alone removes an old link and writes no drop-in" "none none" "$(wiring)"
oldlink dropin
on_host unithost-a installer_retire_local_service 0 "$d/prefix" 0
check "and removes the drop-in" "none none" "$(wiring)"
rm -f "$u/sotd.service.d/topology.conf"

d="$WORK/retire-daemon"; pin_fixture "$d"
( HOME="$d/home"; PATH="$d/stubs:$PATH"; installer_retire_local_service 1 "$d/prefix" 1 ) >/dev/null 2>&1 || true
check "a box that runs a daemon retires nothing" "0" "$(wc -l < "$d/log" | tr -d ' ')"

# ---------------------------------------------------------------------------
case_start "an_unreadable_hosts_toml_is_still_a_declared_topology"
status_stub() {  # <dir> <exit> <stderr line>: a sotd whose `topology status` fails or succeeds as told
    mkdir -p "$1/prefix/bin"
    printf '#!/bin/sh\necho "%s" >&2\nexit %s\n' "$3" "$2" > "$1/prefix/bin/sotd"
    chmod +x "$1/prefix/bin/sotd"
}
d="$WORK/unreadable"; status_stub "$d" 2 "sotd topology: hosts.toml line 2: expected key = value"
rc=0; out="$(installer_topology_unreadable "$d/prefix/bin/sotd")" || rc=$?
check "an invalid hosts.toml is a declared topology" "0" "$rc"
check "its error is reported" "yes" "$(case "$out" in (*"line 2"*) echo yes ;; (*) echo no ;; esac)"
d="$WORK/missing"; status_stub "$d" 2 "sotd topology: no hosts.toml at /x/hosts.toml (run sotd topology sync --hub ALIAS)"
rc=0; installer_topology_unreadable "$d/prefix/bin/sotd" >/dev/null || rc=$?; check "a missing hosts.toml is no topology" "1" "$rc"
d="$WORK/readable"; status_stub "$d" 0 ""
rc=0; installer_topology_unreadable "$d/prefix/bin/sotd" >/dev/null || rc=$?; check "a readable hosts.toml is not unreadable" "1" "$rc"

# ---------------------------------------------------------------------------
case_start "update_rerender_pins_the_unit_before_the_reload"
d="$WORK/rerender"; pin_fixture "$d"
mkdir -p "$d/co/deploy"; cp "$TEMPLATE" "$d/co/deploy/sotd.service"
printf '{\n  "service": "systemd"\n}\n' > "$d/prefix/install.json"
printf '[Service]\nExecStart=%s/bin/sotd --x\n' "$d/prefix" > "$d/home/.config/systemd/user/sotd.service"
( HOME="$d/home"; PATH="$d/stubs:$TOOLS"; sot_rerender_owned "$d/prefix" linux-x86_64 "$d/co" ) >/dev/null 2>&1 || true
pin="$(at "$d" "$(PIN_LINE "$d")")"; reload="$(at "$d" 'systemctl --user daemon-reload')"
check "the unit is re-rendered" "1" "$(grep -c '^Restart=on-failure$' "$d/home/.config/systemd/user/sotd.service" || true)"
linux_check "pin, then reload" "yes" "$([ "${pin:-0}" -gt 0 ] && [ "$pin" -lt "${reload:-0}" ] && echo yes || echo no)"

# ---------------------------------------------------------------------------
case_start "update_rerender_moves_boot_start_to_the_drop_in"
rerender() {  # <dir> <setup: link|dropin|none>: an update over a unit an earlier install left as setup says
    local d="$1" u="$1/home/.config/systemd/user"; pin_fixture "$d"
    mkdir -p "$d/co/deploy"; cp "$TEMPLATE" "$d/co/deploy/sotd.service"
    printf '{\n  "service": "systemd"\n}\n' > "$d/prefix/install.json"
    printf '[Service]\nExecStart=%s/bin/sotd --x\n' "$d/prefix" > "$u/sotd.service"
    case "$2" in
        link) mkdir -p "$u/default.target.wants"; ln -s ../sotd.service "$u/default.target.wants/sotd.service" ;;
        dropin) mkdir -p "$u/default.target.d"; printf '[Unit]\nWants=sotd.service\n' > "$u/default.target.d/sotd.conf" ;;
    esac
    ( HOME="$d/home"; PATH="$d/stubs:$TOOLS"; sot_rerender_owned "$d/prefix" linux-x86_64 "$d/co" ) >/dev/null 2>&1 || true
}
has() { [ -e "$1" ] || [ -L "$1" ] && echo yes || echo no; }
d="$WORK/migrate-link"; rerender "$d" link; u="$d/home/.config/systemd/user"
check "an enabled unit gets the drop-in and loses its link" "yes no" "$(has "$u/default.target.d/sotd.conf") $(has "$u/default.target.wants/sotd.service")"
d="$WORK/migrate-link-real"; pin_fixture "$d"; u="$d/home/.config/systemd/user"
mkdir -p "$u/default.target.wants" "$d/other"; : > "$d/other/keep"
ln -s "$d/other/keep" "$u/default.target.wants/sotd.service"
( sot_boot_wants "$u" ) >/dev/null 2>&1
check "only the link goes: its target stays" "yes no" "$(has "$d/other/keep") $(has "$u/default.target.wants/sotd.service")"
rm -f "$u/default.target.wants/sotd.service"; mkdir "$u/default.target.wants/sotd.service"
( sot_boot_wants "$u" ) >/dev/null 2>&1
check "a directory under that name is not removed" "yes" "$(has "$u/default.target.wants/sotd.service")"
d="$WORK/migrate-none"; rerender "$d" none; u="$d/home/.config/systemd/user"
check "a unit with boot start turned off gets neither" "no no" "$(has "$u/default.target.d/sotd.conf") $(has "$u/default.target.wants/sotd.service")"

# ---------------------------------------------------------------------------
case_start "a_hand_enable_or_disable_on_another_host_changes_no_file"
# Host A's real install writes the folder; host B, which shares the home, then types enable and disable by hand with the
# real systemctl against a copy of it. Linux only: no other platform writes a unit.
if [ "$(uname -s)" = Linux ] && command -v systemd >/dev/null 2>&1; then
    d="$WORK/hand-edit"; pin_fixture "$d"
    ( HOME="$d/home"; PATH="$d/stubs:$PATH"; installer_enable_local_service "$d/prefix" "$TEMPLATE" "$d/sot.sock" ) >/dev/null 2>&1 || true
    r="$d/root"; mkdir -p "$r/etc/systemd"; cp -R "$d/home/.config/systemd/user" "$r/etc/systemd/user"
    real_systemctl="$(type -P systemctl)"
    before="$(tree_of "$r/etc/systemd/user")"
    for verb in "enable" "disable" "disable --now" "enable --now"; do
        # shellcheck disable=SC2086
        "$real_systemctl" --root="$r" --global $verb sotd.service >> "$d/verbs.out" 2>&1 || true
    done
    check "the verbs reached systemd's install logic: it says the unit has no installation config" "yes" \
        "$(grep -q 'no installation config' "$d/verbs.out" && echo yes || echo no)"
    check "host B's enable, disable, disable --now and enable --now change no file" "$before" "$(tree_of "$r/etc/systemd/user")"
    check "the folder holds the boot drop-in" "yes" "$([ -f "$r/etc/systemd/user/default.target.d/sotd.conf" ] && echo yes || echo no)"
    # systemd reads only the scratch folder: HOME and the XDG folders are scratch too, so no unit of the developer's
    # ~/.config/systemd/user is added by the trailing ':' that keeps systemd's own folders.
    wants() {  # <unit folder>: the number of `Wants: sotd.service` lines default.target gets from that folder
        local h="$d/sysd-home"; mkdir -p "$h/config" "$h/data" "$h/run"; chmod 700 "$h/run"
        env -i PATH=/usr/bin:/bin HOME="$h" XDG_CONFIG_HOME="$h/config" XDG_DATA_HOME="$h/data" XDG_RUNTIME_DIR="$h/run" \
            SYSTEMD_UNIT_PATH="$1:" "$(type -P systemd)" --test --user --no-pager --unit=default.target > "$d/test.out" 2>&1 || true
        grep -c -E '^[[:space:]]+Wants: sotd.service' "$d/test.out" || true
    }
    check "default.target still wants sotd.service" "1" "$(wants "$r/etc/systemd/user")"
    mkdir -p "$d/nodrop" && cp "$r/etc/systemd/user/sotd.service" "$d/nodrop/"
    check "the control: the same folder without the drop-in gives no Wants line" "0" "$(wants "$d/nodrop")"
else
    printf '    SKIP a hand enable or disable (Linux with systemd only)\n'
fi

# ---------------------------------------------------------------------------
case_start "record_decision_table"
check "the unit is recorded only for a Linux daemon with a service" "systemd none none none" \
    "$(installer_service_record Linux 1 0) $(installer_service_record Darwin 1 0) $(installer_service_record Linux 0 0) $(installer_service_record Linux 1 1)"
rec() { installer_manifest_json /p /c "$1" 1.0.0 v1.0.0 abc 2026-10-09T00:00:00Z "" "$2" 0 > "$WORK/rec.json"; }
verdict() { installer_record_decision "$@" | cut -d: -f1; }
rec systemd 1
check "no hosts.toml: the record is this box's" allow "$(verdict "$WORK/rec.json" 0 0 none 0)"
check "a host that runs no daemon over a daemon install" refuse "$(verdict "$WORK/rec.json" 1 0 none 0)"
check "a daemon host with --no-service over a unit install" refuse "$(verdict "$WORK/rec.json" 1 1 none 0)"
check "consent" allow "$(verdict "$WORK/rec.json" 1 0 none 1)"
check "a daemon host upgrading" allow "$(verdict "$WORK/rec.json" 1 1 systemd 0)"
check "no record yet" allow "$(verdict "$WORK/absent.json" 1 0 none 0)"
rec none 0
check "a shell install over a shell install" allow "$(verdict "$WORK/rec.json" 1 0 none 0)"

# ---------------------------------------------------------------------------
case_start "host_decision_table"
mkdir -p "$WORK/installed/bin" "$WORK/old/bin" "$WORK/empty"; : > "$WORK/installed/install.json"; : > "$WORK/old/bin/sotd"
hv() { installer_host_decision "$@" | cut -d: -f1; }
check "a host the topology runs sotd on" allow "$(hv 1 'daemon:1 frontend:0' "$WORK/installed" 0)"
check "a frontend host runs its own sotd" allow "$(hv 1 'daemon:1 frontend:1' "$WORK/installed" 0)"
check "an unlisted host over an install" refuse "$(hv 1 none "$WORK/installed" 0)"
check "a host listed with neither key over an install" refuse "$(hv 1 'daemon:0 frontend:0' "$WORK/installed" 0)"
check "an install with no record yet" refuse "$(hv 1 none "$WORK/old" 0)"
check "a first install" allow "$(hv 1 none "$WORK/empty" 0)"
check "consent" allow "$(hv 1 none "$WORK/installed" 1)"
check "no hosts.toml, or one sotd cannot read" allow "$(hv 0 none "$WORK/installed" 0)"

# ---------------------------------------------------------------------------
case_start "an_install_from_a_host_that_runs_no_daemon_changes_nothing_in_the_shared_install"
# A daemon host's install, as an update leaves it: binaries, their .prev rollback copies, rollback state and the record.
# install.sh then runs end to end from a stub release on a host the declared topology runs no sotd on.
me="$(hostname | cut -d. -f1 | tr '[:upper:]' '[:lower:]')"
snapshot() { tree_of "$1/prefix" "$1/home/.local"; }  # every entry of every type, and every file's hash
shared_install_run() {  # <dir> <topology status lines> <mode of the release's sotd> <install.sh args...>
    local d="$1" table="$2" mode="$3"; shift 3
    local rel="$d/rel/sot-9.9.9-$TGT"
    mkdir -p "$d/home" "$d/prefix/bin" "$d/prefix/updates" "$rel" "$d/stubs"
    for b in sot sotd sot-capsule sot-apply; do
        printf 'daemon host %s\n' "$b" > "$d/prefix/bin/$b"; printf 'previous %s\n' "$b" > "$d/prefix/bin/$b.prev"
    done
    printf '{"tag": "v9.9.7"}\n' > "$d/prefix/updates/last-good-$TGT.json"
    # A pointer armed over this install, and another platform's on the same home.
    printf '{"tag": "v9.9.9"}\n' > "$d/prefix/updates/pending-$TGT.json"
    printf '{"tag": "v9.9.9"}\n' > "$d/prefix/updates/pending-other-target.json"
    installer_manifest_json "$d/prefix" "$d/home/.config/sot" systemd 9.9.8 v9.9.8 abc 2026-10-09T00:00:00Z "" 1 0 \
        > "$d/prefix/install.json"
    snapshot "$d" > "$d/before"
    cat > "$rel/sotd" <<STUBEOF
#!/bin/sh
case "\$1 \$2" in
    "topology status") printf 'HOST DECLARED\n$table\n' ;;
    "session-socket-path sot") echo "$d/sot.sock" ;;
    *) echo "sotd 9.9.9" ;;
esac
STUBEOF
    for b in sot sot-capsule sot-apply; do printf '#!/bin/sh\nexit 0\n' > "$rel/$b"; done
    chmod +x "$rel"/*; chmod "$mode" "$rel/sotd"
    tar -czf "$d/rel/sot-9.9.9-$TGT.tar.gz" -C "$d/rel" "sot-9.9.9-$TGT"
    ( cd "$d/rel" && sha256_of "sot-9.9.9-$TGT.tar.gz" > SHA256SUMS )
    # curl and git record their argv in net.log: the stand-ins the network-bound case below reads. git answers a
    # status with a clean tree and a rev-parse with one commit, so a run with a checkout in place reaches Julia.
    cat > "$d/stubs/curl" <<STUBEOF
#!/bin/sh
echo "curl \$*" >> "$d/net.log"
out=""; url=""
while [ \$# -gt 0 ]; do case "\$1" in -o) out="\$2"; shift ;; http*) url="\$1" ;; esac; shift; done
case "\$url" in
    */releases/download/*) cp "$d/rel/\${url##*/}" "\$out" ;;
    */releases/latest) printf 'https://github.com/x/releases/tag/v9.9.9' ;;
    */scripts/install.sh) printf 'exit 0\n' > "\$out" ;;
esac
STUBEOF
    cat > "$d/stubs/git" <<STUBEOF
#!/bin/sh
echo "git \$*" >> "$d/net.log"
case "\$*" in *"status --porcelain"*) ;; *rev-parse*) echo 0123abc ;; esac
STUBEOF
    printf '#!/bin/sh\nexit 3\n' > "$d/stubs/systemctl"
    chmod +x "$d/stubs/curl" "$d/stubs/git" "$d/stubs/systemctl"
    env -i HOME="$d/home" PATH="$d/stubs:$INSTALL_TOOLS" SOT_INSTALL_TAG=v9.9.9 \
        bash "$(dirname "$0")/../install.sh" --prefix "$d/prefix" "$@" 2>&1
}
says() { printf '%s\n' "$1" | grep -c "$2" || true; }
d="$WORK/neither"; rc=0; out="$(shared_install_run "$d" "hub-box hub,daemon\n$me shell" 755 --be-only --no-service)" || rc=$?
check "a host listed with neither key is refused" "2" "$rc"
check "it says why" "1" "$(says "$out" 'hosts.toml runs no sotd on this host, and .* already holds an install')"
check "the shared install is unchanged" "$(cat "$d/before")" "$(snapshot "$d")"
d="$WORK/unlisted"; rc=0; out="$(shared_install_run "$d" "hub-box hub,daemon" 755 --be-only)" || rc=$?
check "an unlisted host asking for a daemon is refused" "2" "$rc"
check "it says why" "1" "$(says "$out" 'hosts.toml runs no sotd on this host, and .* already holds an install')"
check "it names the remedy first" "1" "$(says "$out" "Declare this host in hosts.toml .*hub's hosts.toml.* and re-run")"
check "the shared install is unchanged" "$(cat "$d/before")" "$(snapshot "$d")"
d="$WORK/unrunnable"; rc=0; out="$(shared_install_run "$d" "$me daemon" 644 --be-only)" || rc=$?
check "an unpacked sotd that cannot run stops the install" "1" "$rc"
check "it prints the loader's error" "1" "$(says "$out" 'Permission denied')"
check "it does not blame TMPDIR" "0" "$(says "$out" 'noexec')"
check "the install is unchanged" "$(cat "$d/before")" "$(snapshot "$d")"

# ---------------------------------------------------------------------------
case_start "an_install_without_jq_stops_before_it_downloads_or_changes_anything"
# The same run with jq off install.sh's PATH: the preflight names the command that installs jq on this OS, and no curl
# call is made and nothing under the prefix or ~/.local changes.
NOJQ_TOOLS="$WORK/install-tools-nojq"; mk_tools "$NOJQ_TOOLS" tar gzip tr
case "$(uname -s)" in Darwin) jq_install="brew install jq" ;; *) jq_install="sudo apt-get install jq" ;; esac
d="$WORK/nojq"; rc=0; out="$(INSTALL_TOOLS="$NOJQ_TOOLS" shared_install_run "$d" "$me daemon" 755 --be-only)" || rc=$?
check "an install without jq stops" "1" "$rc"
check "its one jq line names the command that installs it" "1" \
    "$(says "$out" "sot-comm needs jq, which is not on PATH: install it with .$jq_install., then run this installer again")"
check "it downloads nothing" "0" "$(cat "$d/net.log" 2>/dev/null | grep -c '^curl ' || true)"
check "the install is unchanged" "$(cat "$d/before")" "$(snapshot "$d")"

# ---------------------------------------------------------------------------
case_start "every network call the installer makes carries its stall bound"
# The stand-ins of the cases above log their argv. A run with the tag's checkout in place clones the base, fetches the
# tag and asks for Julia; a run with no pinned tag resolves the latest release and fetches that tag's installer.
d="$WORK/net-bounds"; mkdir -p "$d/prefix/repo/versions/v9.9.9/scripts/lib"
cp "$(dirname "$0")/../lib/sot-daemon.sh" "$d/prefix/repo/versions/v9.9.9/scripts/lib/sot-daemon.sh"
rc=0; out="$(shared_install_run "$d" "$me daemon" 755 --be-only --no-service --force-role-change)" || rc=$?
check "a pinned run goes on to ask for Julia" "1" "$(says "$out" 'Julia install failed')"
check "the install drops the pointer armed over the install it replaced, and keeps another platform's" "gone kept" \
    "$([ -e "$d/prefix/updates/pending-$TGT.json" ] && echo kept || echo gone) $([ -e "$d/prefix/updates/pending-other-target.json" ] && echo kept || echo gone)"
env -i HOME="$d/home" PATH="$d/stubs:$INSTALL_TOOLS" bash "$(dirname "$0")/../install.sh" --be-only --prefix "$d/prefix" >/dev/null 2>&1 || true
check "the log holds the prelude, the downloads and the Julia installer's curl calls" "yes" \
    "$([ "$(grep -c '^curl ' "$d/net.log")" -ge 6 ] && echo yes || echo no)"
check "every curl call carries the connect and stall bound" "0" \
    "$(grep '^curl ' "$d/net.log" | grep -vc -e '--connect-timeout 15 --speed-limit 1000 --speed-time 60' || true)"
check "the log holds the base clone and the tag fetch" "1 1" \
    "$(grep -c ' clone ' "$d/net.log") $(grep -c ' fetch --tags' "$d/net.log")"
check "every git network call carries the stall bound" "0" \
    "$(grep -e ' fetch ' -e ' clone ' "$d/net.log" | grep -vc -e '-c http.lowSpeedLimit=1000 -c http.lowSpeedTime=60' || true)"

# ---------------------------------------------------------------------------
printf '\n'
if [ "$fails" -eq 0 ]; then
    printf 'installer-shared-home: all checks passed\n'
else
    printf 'installer-shared-home: %d check(s) FAILED\n' "$fails" >&2
    exit 1
fi
