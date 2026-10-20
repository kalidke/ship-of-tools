# installer-entry-exits.sh -- sourced by installer-entry.sh (its fixture, runner and helpers), never run: every exit of
# the installer ends in one verdict line: an option error, a command that fails under set -e, and an interrupt mid-run,
# whose signal ends the run with its scratch folder and the file a Julia step left in it gone.

# ---------------------------------------------------------------------------
case_start "entry_ends_every_exit_in_one_verdict_line"
# One EXIT trap prints FAIL when no verdict was printed: option errors, a command that fails under set -e, and an interrupt.
verdicts() { printf '%s\n' "$ENTRY_OUT" | grep -c -E '^(PASS|FAIL|ONE STEP LEFT):' || true; }
d="$WORK/vl1"; entry_fixture "$d"
entry_archive "$d" --be-only --bogus
check "an unknown option: exit 2, one verdict line, the last" "2 1 yes" "$ENTRY_RC $(verdicts) $(case "$ENTRY_LAST" in ('FAIL: '*'unknown flag: --bogus') echo yes ;; (*) echo no ;; esac)"
d="$WORK/vl2"; entry_fixture "$d"
entry_record "$d" 9.9.8 1 0 systemd
cat > "$d/stubs/systemctl" <<'SC'
#!/bin/sh
echo "systemctl $*" >> "$CTL/systemctl.log"
case "$*" in
    "--user is-active"*) exit 3 ;;
    "--user daemon-reload"|"--user start sotd.service") echo "Failed to connect to bus: No medium found" >&2; exit 1 ;;
esac
exit 0
SC
entry_archive "$d" --be-only
check "a user manager that cannot be reached: exit 1, one verdict line, the last" "1 1 FAIL: service: stopped (exit 1)" "$ENTRY_RC $(verdicts) $ENTRY_LAST"
d="$WORK/vl3"; entry_fixture "$d"
entry_run "$d" SOT_INSTALL_TAG=v9.9.9 --be-only --version
check "--version with no value: exit 2, one verdict line" "2 1" "$ENTRY_RC $(verdicts)"
starts_with "and it says which option" "FAIL: " "$ENTRY_LAST"
check "naming it" "1" "$(said '--version needs a value')"
# An interrupt mid-run, during a Julia step that leaves a file in its temporary folder, found by TMPDIR as Julia finds it
# (as Pkg leaves a download there when it is interrupted): the signal ends the run after one verdict line, and the
# scratch folder is gone with that file.
d="$WORK/vl4"; entry_fixture "$d"
printf 'f="$(mktemp "${TMPDIR:-/tmp}/left.XXXXXX")" && echo "$f" > "$CTL/left"; sleep 2\n' > "$d/ctl/hook"
env -i HOME="$d/home" PATH="$d/stubs:$ENTRY_TOOLS" CTL="$d/ctl" SOT_COMM_HOME="$d/comm" SOT_SELF_HOST=entrybox TMPDIR="$d" \
    bash "$ENTRY_SRC/scripts/install.sh" --archive "$d/rel" --be-only --no-service > "$d/sig.out" 2>&1 &
sig_pid=$!
n=0; while [ "$n" -lt 150 ] && ! grep -q update_comm "$d/ctl/julia.log" 2>/dev/null; do sleep 0.1; n=$((n + 1)); done
kill -TERM "$sig_pid"
sig_rc=0; wait "$sig_pid" || sig_rc=$?
check "a TERM mid-run ends by the signal (143)" "143" "$sig_rc"
check "with one verdict line, the last, naming the stage" "1 FAIL: comm: stopped (exit 143)" "$(grep -c -E '^(PASS|FAIL|ONE STEP LEFT):' "$d/sig.out" || true) $(tail -1 "$d/sig.out")"
check "and its scratch folder is gone" "0" "$(ls "$d" | grep -c '^sot-install\.' || true)"
check "and so is the file the Julia step left" "gone" "$(f="$(cat "$d/ctl/left" 2>/dev/null)"; [ -z "$f" ] && echo "none named" || { [ -e "$f" ] && echo "left: $f" || echo gone; })"
