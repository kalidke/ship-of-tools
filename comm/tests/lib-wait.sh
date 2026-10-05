# lib-wait.sh — the waits the comm and agents suites share (comm/tests/CLAUDE.md's timing rule). A suite sources it
# after lib-home-guard.sh. Functions only; sourcing runs nothing.

# await CMD [ARG...] — run CMD every 50 ms until it succeeds; 1 if it has not after 600 tries. The 30 s is a hang
# guard: under the one-CPU driver the slowest step measured needed 19 tries.
await() {
    local i
    for i in $(seq 600); do
        "$@" && return 0
        sleep 0.05
    done
    return 1
}

# stopped PID — 0 when PID has stopped itself (`kill -STOP`): everything it did before that is done, and a CONT sent
# now cannot arrive before its STOP.
stopped() {
    case "$(ps -o stat= -p "$1" 2>/dev/null)" in *T*) return 0 ;; esac
    return 1
}

# sleep_log DIR LOG — make DIR/sleep, a `sleep` that appends its arguments to LOG and then runs the real sleep, whose
# path is resolved here, before DIR is first on any PATH. A case puts DIR first on one command's PATH and counts the
# waits that command made; a stub that must itself sleep calls the real sleep by its path.
sleep_log() {
    local real
    real="$(command -v sleep)" || return 1
    mkdir -p "$1" || return 1
    printf '#!/usr/bin/env bash\necho "$*" >> "%s"\nexec "%s" "$@"\n' "$2" "$real" > "$1/sleep" && chmod +x "$1/sleep"
}
