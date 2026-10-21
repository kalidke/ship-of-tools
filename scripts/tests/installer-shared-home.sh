#!/usr/bin/env bash
# installer-shared-home.sh -- a home several hosts share holds one install: an update's re-render of sotd.service pins it
# to the hosts that run sotd (`sotd topology pin`) before any reload and moves boot start onto the drop-in, and a hand
# enable or disable on another host changes no file. The library runs for real; `sotd`, `systemctl` and `loginctl` are
# stubs and HOME is scratch. The install's own wiring and retire are sotd install's unit tests
# (rust/backend/src/install/unix_tests.rs). Run: scripts/tests/installer-shared-home.sh

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
# The line number of the first log line equal to <line>, or 0.
at() { grep -n -x -F -- "$2" "$1/log" | head -1 | cut -d: -f1 || true; }
PIN_LINE() { printf 'sotd topology pin --dir %s/home/.config/systemd/user' "$1"; }

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
# Host A's install writes the folder; host B, which shares the home, then types enable and disable by hand with the
# real systemctl against a copy of it. Linux only: no other platform writes a unit.
if [ "$(uname -s)" = Linux ] && command -v systemd >/dev/null 2>&1; then
    # The folder as an install leaves it: the rendered unit, its host pin and the boot drop-in.
    d="$WORK/hand-edit"; pin_fixture "$d"; u="$d/home/.config/systemd/user"
    ( HOME="$d/home"; render_sotd_unit "$d/prefix" "$TEMPLATE" "$u/sotd.service" && sot_boot_wants "$u" ) >/dev/null 2>&1 || true
    mkdir -p "$u/sotd.service.d"; printf '[Unit]\nConditionHost=|unithost-a\nConditionHost=|unithost-a.*\n' > "$u/sotd.service.d/topology.conf"
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
printf '\n'
if [ "$fails" -eq 0 ]; then
    printf 'installer-shared-home: all checks passed\n'
else
    printf 'installer-shared-home: %d check(s) FAILED\n' "$fails" >&2
    exit 1
fi
