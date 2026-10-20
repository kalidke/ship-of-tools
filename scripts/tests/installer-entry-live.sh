# installer-entry-live.sh -- sourced by installer-entry.sh (its fixture, runner and helpers), never run: the ask judges "live"
# from the daemon's own process tree. An old daemon that cannot report its Pluto server, or be told its version, and has no
# REPL or Pluto child installs in one plain run; a REPL or Pluto child is live (no console: the one-line stop; a console:
# the question in place, y goes on and anything else stops).

# ---------------------------------------------------------------------------
case_start "entry_judges_live_from_the_daemons_children"
# The old daemon that cannot report its Pluto server: its own process tree decides what is live. A kernel alone is nothing
# live, so one plain run installs, no flag and no stop. A REPL child or the Pluto server is live: with no console the
# one-line stop, with a console the question in place (y goes on, anything else stops).
asus_fixture() {  # <d> <child line of the daemon's process tree>
    local d="$1"
    entry_fixture "$d"
    entry_record "$d" 9.9.8 1 0 systemd; : > "$d/ctl/unit-active"; : > "$d/ctl/restart-same"
    printf '9.9.8\n' > "$d/ctl/same-out"; printf '3\n' > "$d/ctl/same-rc"
    : > "$d/ctl/repls-out"; printf '%s\n' "$LIVE_PLUTO_OLD" > "$d/ctl/repls-err"; printf '1\n' > "$d/ctl/repls-rc"
    printf '4242 sotd %s/.local/share/sot/bin/sotd\n' "$d/home" > "$d/ctl/ps"
    printf '4242 1 %s/.local/share/sot/bin/sotd\n%s\n' "$d/home" "$2" > "$d/ctl/ps-tree"
}
d="$WORK/live-asus"; asus_fixture "$d" '4300 4242 julia --startup-file=no -e using ShipToolsKernel; ShipToolsKernel.serve(stdin, stdout)'
entry_archive "$d" --be-only
check "an old daemon that cannot report Pluto, no REPL or Pluto child: no flag, no stop, the unit restarts, PASS" "0 1 PASS" "$ENTRY_RC $(count_in "$d/ctl/systemctl.log" 'try-restart') $(printf '%s' "$ENTRY_LAST" | cut -c1-4)"
check "and it says it found nothing live to end" "1" "$(said 'it has no REPL or Pluto child, so restarting it ends nothing live')"
d="$WORK/live-asus-repl"; asus_fixture "$d" '4300 4242 julia --project=/w -e using ShipToolsRepl; ShipToolsRepl.run_shim(stdin, stdout)'
before="$(entry_prefix_tree "$d")"
entry_archive "$d" --be-only
check "a REPL child and no console: the FAIL, no restart, nothing changed" "2 0 1 $before" "$ENTRY_RC $(count_in "$d/ctl/systemctl.log" 'try-restart') $(said "FAIL: daemon: could not ask the daemon on $d/ctl/sot.sock in full: it runs another version; $LIVE_PLUTO_OLD") $(entry_prefix_tree "$d")"
printf 'y\n' > "$d/ctl/tty"
entry_archive "$d" SOT_INSTALL_TTY="$d/ctl/tty" --be-only
check "a console answering y: exit 0, the unit restarted, PASS" "0 1 PASS" "$ENTRY_RC $(count_in "$d/ctl/systemctl.log" 'try-restart') $(printf '%s' "$ENTRY_LAST" | cut -c1-4)"
check "the question named what the restart ends" "1" "$(grep -c -F 'Restart the daemon? This ends a live REPL (the daemon could not name its row), and any other the daemon could not list [y/N]' "$d/ctl/tty" || true)"
d="$WORK/live-asus-pluto"; asus_fixture "$d" '4301 4242 julia --project=/p /x/julia/pluto/start.jl'
entry_archive "$d" --be-only
check "a Pluto server child and no console: the FAIL too" "2 0" "$ENTRY_RC $(count_in "$d/ctl/systemctl.log" 'try-restart')"
d="$WORK/live-asus-n"; asus_fixture "$d" '4300 4242 julia --project=/w -e using ShipToolsRepl; ShipToolsRepl.run_shim(stdin, stdout)'
before="$(entry_prefix_tree "$d")"
printf 'n\n' > "$d/ctl/tty"
entry_archive "$d" SOT_INSTALL_TTY="$d/ctl/tty" --be-only
check "a console answering n: exit 2, no restart, nothing changed" "2 0 $before" "$ENTRY_RC $(count_in "$d/ctl/systemctl.log" 'try-restart') $(entry_prefix_tree "$d")"
# The same when the daemon cannot even be told its version.
d="$WORK/live-asus-nover"; asus_fixture "$d" '4300 4242 julia --startup-file=no -e using ShipToolsKernel; ShipToolsKernel.serve(stdin, stdout)'
printf '1\n' > "$d/ctl/same-rc"; printf 'unix:/x: no reply\n' > "$d/ctl/same-err"
entry_archive "$d" --be-only
check "a daemon that cannot be told its version and has no REPL or Pluto child: no flag, no stop, PASS" "0 1" "$ENTRY_RC $(count_in "$d/ctl/systemctl.log" 'try-restart')"
d="$WORK/live-asus-nover-repl"; asus_fixture "$d" '4300 4242 julia -e using ShipToolsRepl; ShipToolsRepl.run_shim(stdin, stdout)'
printf '1\n' > "$d/ctl/same-rc"; printf 'unix:/x: no reply\n' > "$d/ctl/same-err"
entry_archive "$d" --be-only
check "the same with a REPL child and no console: the FAIL" "2 0 1" "$ENTRY_RC $(count_in "$d/ctl/systemctl.log" 'try-restart') $(said "FAIL: daemon: a daemon runs on $d/ctl/sot.sock and could not be told its version")"
printf 'y\n' > "$d/ctl/tty"
entry_archive "$d" SOT_INSTALL_TTY="$d/ctl/tty" --be-only
check "and with a console answering y it restarts and goes on" "0 1" "$ENTRY_RC $(count_in "$d/ctl/systemctl.log" 'try-restart')"
