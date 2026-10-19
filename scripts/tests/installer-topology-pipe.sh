# installer-topology-pipe.sh -- sourced by installer-state.sh (its checks and STATUS_TABLE), never run: awk exits at the
# matching line, and a table far larger than a pipe holds leaves any writer still writing then; under pipefail its SIGPIPE
# failed a pipe-fed installer_topology_role (exit 141). The status must be 0 and the answer whole on every run, not 1 in 4.
BIG_TABLE="$STATUS_TABLE$(awk 'BEGIN { for (i = 0; i < 40000; i++) printf "\nfiller-%d shell", i }')"
for run in 1 2 3 4 5 6 7 8; do
    rc=0; got="$(installer_topology_role "$BIG_TABLE" laptop)" || rc=$?
    check "a match followed by a table far larger than a pipe (run $run)" "daemon:1 frontend:1 rc=0" "$got rc=$rc"
done
