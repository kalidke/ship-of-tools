# installer-apply-boot.sh -- sourced by installer-apply.sh (its fixtures and helpers), never run: the background update
# says, once and in the installer's own words, when it leaves a managed unit's boot start off.

# ---------------------------------------------------------------------------
case_start "apply_says_when_it_leaves_boot_start_off"
BOOT_OFF_LINE='sotd.service: boot start is off on this computer; turn it on with: mkdir -p ~/.config/systemd/user/default.target.d && printf '
boot_off_count() { grep -cF "sot-apply: $BOOT_OFF_LINE" "$1/out" || true; }
d="$WORK/ap-bo1"; mk_apply_fixture "$d" 1 1; run_apply "$d"
check "an owned unit with neither link nor drop-in: the line once" "1" "$(boot_off_count "$d")"
check "it follows the APPLIED line" "APPLIED" "$(grep -A1 'sot-apply: APPLIED' "$d/out" | sed -n 2p | grep -q 'boot start is off' && echo APPLIED)"
check "it equals install.sh's line, one library function" "same" \
    "$( (. "$SRC/scripts/lib/sot-daemon.sh"; sot_boot_off_line) > "$d/want"; sed -n 's/^sot-apply: \(sotd.service: boot start is off.*\)$/\1/p' "$d/out" | cmp -s - "$d/want" && [ -s "$d/want" ] && echo same || echo differ)"
d="$WORK/ap-bo2"; mk_apply_fixture "$d" 1 1; mkdir -p "$d/home/.config/systemd/user/default.target.d"
printf '[Unit]\nWants=sotd.service\n' > "$d/home/.config/systemd/user/default.target.d/sotd.conf"; run_apply "$d"
check "with the drop-in: no line" "0" "$(boot_off_count "$d")"
d="$WORK/ap-bo3"; mk_apply_fixture "$d" 1 1; seed_link "$d"; run_apply "$d"
check "with the old link: no line" "0" "$(boot_off_count "$d")"
d="$WORK/ap-bo4"; mk_apply_fixture "$d" 0 0; run_apply "$d"
check "with no managed unit: no line" "0" "$(boot_off_count "$d")"
