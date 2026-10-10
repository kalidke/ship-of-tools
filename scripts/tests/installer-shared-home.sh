#!/usr/bin/env bash
# installer-shared-home.sh -- a home several hosts share holds one install: every path that writes, re-renders or
# retires sotd.service pins it to the hosts that run sotd (`sotd topology pin`) before any reload and never disables the
# shared unit under a declared topology.
# The functions and install.sh run for real; `sotd`, `systemctl`, `loginctl`, `curl` and `git` are stubs, HOME is
# scratch. Run: scripts/tests/installer-shared-home.sh

set -euo pipefail

# shellcheck source=installer-support.sh
. "$(dirname "$0")/installer-support.sh"

TEMPLATE="$(dirname "$0")/../../deploy/sotd.service"

# A scratch home and prefix whose sotd, systemctl and loginctl append their argv to one log, in call order.
pin_fixture() {  # <dir> <unit enabled 0|1>
    local d="$1"
    mkdir -p "$d/home/.config/systemd/user" "$d/prefix/bin" "$d/stubs"
    : > "$d/log"
    printf '#!/bin/sh\nprintf "sotd %%s\\n" "$*" >> "%s/log"\n' "$d" > "$d/prefix/bin/sotd"
    cat > "$d/stubs/systemctl" <<STUBEOF
#!/bin/sh
printf 'systemctl %s\n' "\$*" >> "$d/log"
case "\$*" in *is-enabled*) [ "$2" = 1 ] ;; esac
STUBEOF
    printf '#!/bin/sh\nprintf "loginctl %%s\\n" "$*" >> "%s/log"\n' "$d" > "$d/stubs/loginctl"
    chmod +x "$d/prefix/bin/sotd" "$d/stubs/systemctl" "$d/stubs/loginctl"
}
# The line number of the first log line equal to <line>, or 0.
at() { grep -n -x -F -- "$2" "$1/log" | head -1 | cut -d: -f1 || true; }
PIN_LINE() { printf 'sotd topology pin --dir %s/home/.config/systemd/user' "$1"; }

# ---------------------------------------------------------------------------
case_start "install_pins_the_unit_before_it_is_loaded"
d="$WORK/enable"; pin_fixture "$d" 0
( HOME="$d/home"; PATH="$d/stubs:$PATH"; installer_enable_local_service "$d/prefix" "$TEMPLATE" "$d/sot.sock" ) >/dev/null 2>&1 || true
pin="$(at "$d" "$(PIN_LINE "$d")")"; reload="$(at "$d" 'systemctl --user daemon-reload')"; enable="$(at "$d" 'systemctl --user enable --now sotd.service')"
check "the unit is written" "yes" "$([ -f "$d/home/.config/systemd/user/sotd.service" ] && echo yes || echo no)"
check "the pin is written into the unit's folder" "yes" "$([ "${pin:-0}" -gt 0 ] && echo yes || echo no)"
check "pin, then reload, then enable" "yes" "$([ "${pin:-0}" -gt 0 ] && [ "$pin" -lt "${reload:-0}" ] && [ "$reload" -lt "${enable:-0}" ] && echo yes || echo no)"

# ---------------------------------------------------------------------------
case_start "retire_under_a_topology_pins_and_stops_and_never_disables"
d="$WORK/retire-topo"; pin_fixture "$d" 1
printf '[Service]\nExecStart=/x/bin/sotd\n' > "$d/home/.config/systemd/user/sotd.service"
( HOME="$d/home"; PATH="$d/stubs:$PATH"; installer_retire_local_service 0 "$d/prefix" 1 ) >/dev/null 2>&1 || true
pin="$(at "$d" "$(PIN_LINE "$d")")"; stop="$(at "$d" 'systemctl --user stop sotd.service')"; reload="$(at "$d" 'systemctl --user daemon-reload')"
check "the shared unit is never disabled" "0" "$(grep -c 'disable' "$d/log" || true)"
check "pin, then stop this host's run, then reload" "yes" "$([ "${pin:-0}" -gt 0 ] && [ "$pin" -lt "${stop:-0}" ] && [ "$stop" -lt "${reload:-0}" ] && echo yes || echo no)"
check "the unit file stays" "yes" "$([ -f "$d/home/.config/systemd/user/sotd.service" ] && echo yes || echo no)"

d="$WORK/retire-topo-nounit"; pin_fixture "$d" 0
( HOME="$d/home"; PATH="$d/stubs:$PATH"; installer_retire_local_service 0 "$d/prefix" 1 ) >/dev/null 2>&1 || true
check "with no unit file nothing runs" "0" "$(wc -l < "$d/log" | tr -d ' ')"

# ---------------------------------------------------------------------------
case_start "retire_without_a_topology_disables_as_before"
d="$WORK/retire-flags"; pin_fixture "$d" 1
( HOME="$d/home"; PATH="$d/stubs:$PATH"; installer_retire_local_service 0 "$d/prefix" 0 ) >/dev/null 2>&1 || true
check "the lone box's unit is disabled" "1" "$(grep -c -x -F 'systemctl --user disable --now sotd.service' "$d/log" || true)"
check "no pin is written" "0" "$(grep -c '^sotd ' "$d/log" || true)"

d="$WORK/retire-daemon"; pin_fixture "$d" 1
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
rc=0; out="$(installer_topology_unreadable "$d/prefix")" || rc=$?
check "an invalid hosts.toml is a declared topology" "0" "$rc"
check "its error is reported" "yes" "$(case "$out" in *"line 2"*) echo yes ;; *) echo no ;; esac)"
d="$WORK/missing"; status_stub "$d" 2 "sotd topology: no hosts.toml at /x/hosts.toml (run sotd topology sync --hub ALIAS)"
rc=0; installer_topology_unreadable "$d/prefix" >/dev/null || rc=$?; check "a missing hosts.toml is no topology" "1" "$rc"
d="$WORK/readable"; status_stub "$d" 0 ""
rc=0; installer_topology_unreadable "$d/prefix" >/dev/null || rc=$?; check "a readable hosts.toml is not unreadable" "1" "$rc"

# ---------------------------------------------------------------------------
case_start "update_rerender_pins_the_unit_before_the_reload"
d="$WORK/rerender"; pin_fixture "$d" 0
mkdir -p "$d/co/deploy"; cp "$TEMPLATE" "$d/co/deploy/sotd.service"
printf '{\n  "service": "systemd"\n}\n' > "$d/prefix/install.json"
printf '[Service]\nExecStart=%s/bin/sotd --x\n' "$d/prefix" > "$d/home/.config/systemd/user/sotd.service"
( HOME="$d/home"; PATH="$d/stubs:$TOOLS"; sot_rerender_owned "$d/prefix" linux-x86_64 "$d/co" ) >/dev/null 2>&1 || true
pin="$(at "$d" "$(PIN_LINE "$d")")"; reload="$(at "$d" 'systemctl --user daemon-reload')"
check "the unit is re-rendered" "1" "$(grep -c '^Restart=on-failure$' "$d/home/.config/systemd/user/sotd.service" || true)"
check "pin, then reload" "yes" "$([ "${pin:-0}" -gt 0 ] && [ "$pin" -lt "${reload:-0}" ] && echo yes || echo no)"

# ---------------------------------------------------------------------------
printf '\n'
if [ "$fails" -eq 0 ]; then
    printf 'installer-shared-home: all checks passed\n'
else
    printf 'installer-shared-home: %d check(s) FAILED\n' "$fails" >&2
    exit 1
fi
