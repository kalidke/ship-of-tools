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
rc=0; out="$(installer_topology_unreadable "$d/prefix/bin/sotd")" || rc=$?
check "an invalid hosts.toml is a declared topology" "0" "$rc"
check "its error is reported" "yes" "$(case "$out" in *"line 2"*) echo yes ;; *) echo no ;; esac)"
d="$WORK/missing"; status_stub "$d" 2 "sotd topology: no hosts.toml at /x/hosts.toml (run sotd topology sync --hub ALIAS)"
rc=0; installer_topology_unreadable "$d/prefix/bin/sotd" >/dev/null || rc=$?; check "a missing hosts.toml is no topology" "1" "$rc"
d="$WORK/readable"; status_stub "$d" 0 ""
rc=0; installer_topology_unreadable "$d/prefix/bin/sotd" >/dev/null || rc=$?; check "a readable hosts.toml is not unreadable" "1" "$rc"

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
case_start "an_install_from_a_host_that_runs_no_daemon_changes_nothing_in_the_shared_install"
# A daemon host's install, as an update leaves it: binaries, their .prev rollback copies, rollback state and the record.
# install.sh then runs on a host the declared topology lists with neither key, from a release of the same shape.
d="$WORK/shared"; me="$(hostname | cut -d. -f1 | tr '[:upper:]' '[:lower:]')"; rel="$d/rel/sot-9.9.9-linux-x86_64"
mkdir -p "$d/home" "$d/prefix/bin" "$d/prefix/updates" "$rel" "$d/stubs"
for b in sot sotd sot-capsule sot-apply; do
    printf 'daemon host %s\n' "$b" > "$d/prefix/bin/$b"; printf 'previous %s\n' "$b" > "$d/prefix/bin/$b.prev"
done
printf '{"tag": "v9.9.7"}\n' > "$d/prefix/updates/last-good-linux-x86_64.json"
installer_manifest_json "$d/prefix" "$d/home/.config/sot" systemd 9.9.8 v9.9.8 abc 2026-10-09T00:00:00Z "" 1 0 > "$d/prefix/install.json"
before="$(find "$d/prefix" -type f -exec sha256sum {} + | sort)"
cat > "$rel/sotd" <<STUBEOF
#!/bin/sh
case "\$1 \$2" in
    "topology status") printf 'HOST DECLARED\nhub-box hub,daemon\n%s shell\n' "$me" ;;
    "session-socket-path sot") echo "$d/sot.sock" ;;
    *) echo "sotd 9.9.9" ;;
esac
STUBEOF
for b in sot sot-capsule sot-apply; do printf '#!/bin/sh\nexit 0\n' > "$rel/$b"; done
chmod +x "$rel"/*
tar -czf "$d/rel/sot-9.9.9-linux-x86_64.tar.gz" -C "$d/rel" sot-9.9.9-linux-x86_64
( cd "$d/rel" && sha256sum sot-9.9.9-linux-x86_64.tar.gz > SHA256SUMS )
cat > "$d/stubs/curl" <<STUBEOF
#!/bin/sh
out=""; url=""
while [ \$# -gt 0 ]; do case "\$1" in -o) out="\$2"; shift ;; http*) url="\$1" ;; esac; shift; done
case "\$url" in */releases/download/*) cp "$d/rel/\${url##*/}" "\$out" ;; esac
STUBEOF
printf '#!/bin/sh\nexit 1\n' > "$d/stubs/git"
printf '#!/bin/sh\nexit 3\n' > "$d/stubs/systemctl"
chmod +x "$d/stubs/curl" "$d/stubs/git" "$d/stubs/systemctl"
rc=0
out="$(env -i HOME="$d/home" PATH="$d/stubs:/usr/bin:/bin" SOT_INSTALL_TAG=v9.9.9 \
    bash "$(dirname "$0")/../install.sh" --be-only --prefix "$d/prefix" 2>&1)" || rc=$?
check "the install is refused before it writes" "2" "$rc"
check "it says why" "1" "$(printf '%s\n' "$out" | grep -c 'records an install that runs a daemon, and this run would record none' || true)"
check "the shared install is byte-identical" "$before" "$(find "$d/prefix" -type f -exec sha256sum {} + | sort)"

# ---------------------------------------------------------------------------
printf '\n'
if [ "$fails" -eq 0 ]; then
    printf 'installer-shared-home: all checks passed\n'
else
    printf 'installer-shared-home: %d check(s) FAILED\n' "$fails" >&2
    exit 1
fi
