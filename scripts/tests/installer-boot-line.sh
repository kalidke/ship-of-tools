# installer-boot-line.sh -- sourced by installer-entry.sh (its fixture, runner and helpers), never run: an install over an
# existing sotd.service that has neither the old enable link nor the boot drop-in leaves boot start off, and says so in
# one line before the verdict, naming the command that turns it on; a fresh install, which wires boot start, does not.

# ---------------------------------------------------------------------------
case_start "entry_says_when_it_leaves_boot_start_off"
BOOT_OFF_LINE='sotd.service: boot start is off on this computer; turn it on with: mkdir -p ~/.config/systemd/user/default.target.d && printf '
boot_off_run() {  # <name> <unit-before 0|1>: a run that ends in PASS; sets BO (its verdict word, the boot-off lines) and BO_AT (the line 3 from the end)
    local d="$WORK/bootoff-$1"; entry_fixture "$d"
    entry_record "$d" 9.9.8 1 0 systemd; : > "$d/ctl/unit-active"; : > "$d/ctl/restart-same"
    printf '9.9.8\n' > "$d/ctl/same-out"; printf '3\n' > "$d/ctl/same-rc"
    [ "$2" = 0 ] || printf '[Service]\nExecStart=%s/home/.local/share/sot/bin/sotd\n' "$d" > "$d/home/.config/systemd/user/sotd.service"
    entry_archive "$d" --be-only --restart-daemon
    BO="$(printf '%s' "$ENTRY_LAST" | cut -c1-4) $(said "$BOOT_OFF_LINE")"
    BO_AT="$(printf '%s\n' "$ENTRY_OUT" | tail -3 | head -1 | cut -c1-60)"
}
boot_off_run over 1
check "an install over a unit with neither link nor drop-in ends in PASS with one boot-off line" "PASS 1" "$BO"
check "the line comes with the verdict, before the stop line" "$(printf '%s' "$BOOT_OFF_LINE" | cut -c1-60)" "$BO_AT"
boot_off_run fresh 0
check "a fresh install prints no boot-off line" "PASS 0" "$BO"
