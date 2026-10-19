# installer-ensure-timeout.sh -- sourced by installer-state.sh, never run, after run_ensure and newest_log are defined: the
# message `sot_daemon_ensure` prints when the daemon is alive but has not bound its socket.

# ---------------------------------------------------------------------------
case_start "ensure_names_the_daemons_log_on_a_timeout"
# A daemon alive but not bound has its lines in its own log, which the daemon names (`sotd log-path`).
d="$WORK/timeout"; mkdir -p "$d/home"
STUB_DELAY=30 ENS_WAIT=2 STUB_DAEMON_LOG="$d/state/sotd.log" run_ensure "$d" "$d/prefix" 0
check "a daemon that never binds returns 1" "1" "$ENS_RC"
case "$ENS_ERR" in *"its log: $d/state/sotd.log"*) check "the daemon's log is named" ok ok ;; *) check "the daemon's log is named" "its log: $d/state/sotd.log" "$ENS_ERR" ;; esac
case "$ENS_ERR" in *"see $(newest_log "$d/prefix/logs") (its stderr)"*) check "the start's stderr file is named too" ok ok ;; *) check "the start's stderr file is named too" "see <log> (its stderr)" "$ENS_ERR" ;; esac
reap_stub "$d"
