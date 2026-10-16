# lib-wait.sh — the waits the comm and agents suites share (comm/tests/CLAUDE.md's timing rule), and the way a
# suite ends a step that runs until it is stopped and has children of its own (end_group). A suite sources it after
# lib-home-guard.sh. Functions only; sourcing runs nothing.

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

# end_group PGID — end a step a suite started in the background between `set -m` and `set +m`, so that the step leads
# a process group of its own and PGID is its $!: TERM to the whole group, a wait until the group is empty, KILL to what
# is left when it has not emptied within await's 30 s, then the leader reaped. 1 when KILL was needed; 0 with no PGID.
# A live PGID that leads no group (the step was started outside `set -m`) is a misuse: one stderr line, the pid killed
# and reaped, 2. The wait after KILL has no bound of its own, as a process in uninterruptible sleep survives KILL.
# The group holds every process the step started, a pipeline's members and a child it execs included, and kill reaches
# it on Linux, macOS and Git Bash. A stop by parent id does not: a pipeline's members are not the step's children, and
# Git Bash has no pkill or pgrep, so each member such a stop missed outlived the suite (a bash and a tail per stub daemon
# start).
group_gone() { ! kill -0 -- "-$1" 2>/dev/null; }
end_group() {
    local rc=0
    [ -n "${1:-}" ] || return 0
    if kill -0 "$1" 2>/dev/null && ! kill -0 -- "-$1" 2>/dev/null; then
        echo "end_group: $1 leads no process group; start the step between set -m and set +m" >&2
        kill -KILL "$1" 2>/dev/null; wait "$1" 2>/dev/null
        return 2
    fi
    kill -TERM -- "-$1" 2>/dev/null
    if ! await group_gone "$1"; then
        kill -KILL -- "-$1" 2>/dev/null; rc=1
        # A killed member leaves the group a moment after KILL; a case's stubbed `sleep` must not shorten this wait.
        for _ in $(seq 100); do group_gone "$1" && break; command sleep 0.05; done
    fi
    wait "$1" 2>/dev/null
    return "$rc"
}

# bounded SECS CMD [ARG...] — CMD, ended by TERM after SECS seconds: its own status, or 124 when the bound ended it, as
# GNU timeout(1), which macOS does not ship; on perl, which it does.
bounded() {
    perl -e 'my $s = shift; my $p = fork; defined $p or exit 125; if (!$p) { exec { $ARGV[0] } @ARGV; exit 127 }
        $SIG{ALRM} = sub { kill "TERM", $p; waitpid $p, 0; exit 124 }; alarm $s; waitpid $p, 0;
        exit($? & 127 ? 128 + ($? & 127) : $? >> 8)' "$@"
}

# stamp_ago SECS — the UTC stamp (YYYY-MM-DDTHH:MM:SSZ) SECS seconds ago, without GNU date's -d.
stamp_ago() { perl -MPOSIX=strftime -e 'print strftime("%Y-%m-%dT%H:%M:%SZ", gmtime(time - $ARGV[0])), "\n"' "$1"; }

# linux_only WHAT — 0 on Linux; elsewhere names WHAT as skipped and returns 1: the case tests Linux's inbox lock (flock)
# or reads /proc, and macOS files through the daemon and has no /proc.
linux_only() {
    [ "$(uname -s)" = Linux ] && return 0
    echo "SKIP: $1: Linux's inbox lock (flock) or /proc; macOS files through the daemon"
    return 1
}
