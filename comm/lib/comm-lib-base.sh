# comm-lib-base.sh: the library's base: platform test, the comm folder's paths, clock, tool checks, the command bound, jq and host helpers, ages.
# Sourced by comm-lib.sh; defines functions and globals only, and sets the umask.

# _sot_is_windows — the ONE shared platform test (Codex review, PR1 round 2
# finding 6: Windows-specific defaults/guards must live HERE, not duplicated
# per-caller — a caller-side workaround dies with that process, so a later,
# separately-invoked script (e.g. a retried invocation) never
# sees it and falls through to Linux-only logic that has no role on Windows).
# Every script that sources comm-lib.sh calls this one instead of re-deriving it.
_sot_is_windows() {
    case "${OS:-}" in Windows_NT) return 0 ;; esac
    case "${OSTYPE:-}" in msys*|cygwin*|win32) return 0 ;; esac
    case "$(uname -s 2>/dev/null || true)" in MINGW*|MSYS*|CYGWIN*) return 0 ;; esac
    return 1
}

# The one comm-folder rule, as `sotd comm` reads it (rust/backend/src/comm/mod.rs, `sot_comm_home`), for the scripts
# that name a file in it (a spawn's new inbox, a recovery line).
COMM_HOME="${SOT_COMM_HOME:-$HOME/.sot-comm}"
# Absolute, and exported so, so a later cd or an exported CDPATH cannot make two commands, or a script and its
# children, mean two folders.
case "$COMM_HOME" in /*|[A-Za-z]:*) ;; *) COMM_HOME="$PWD/$COMM_HOME"; export SOT_COMM_HOME="$COMM_HOME" ;; esac
REGISTRY="$COMM_HOME/registry.json"
INBOX_DIR="$COMM_HOME/inbox"
SELF_DIR="$COMM_HOME/self"
READ_DIR="$COMM_HOME/read"
# Everything a comm script makes is its user's alone (ADR 0049, User isolation).
umask 077
# Every shell that loads this library arms its cleanup traps here, before it holds anything (cleanup traps, below).
trap - HUP INT TERM

now_iso() { date -u +%Y-%m-%dT%H:%M:%SZ; }
# sot_epoch STAMP — STAMP (YYYY-MM-DDTHH:MM:SSZ, UTC) in seconds since the epoch, by GNU date's -d or BSD's -j -f
# (macOS); nothing and status 1 when neither reads it. sot_mtime FILE — FILE's modification time the same way, by GNU
# stat's -c or BSD's -f.
sot_epoch() { date -u -d "$1" +%s 2>/dev/null || LC_ALL=C date -u -j -f %Y-%m-%dT%H:%M:%SZ "$1" +%s 2>/dev/null; }
sot_mtime() { stat -c %Y "$1" 2>/dev/null || stat -f %m "$1" 2>/dev/null; }

# sot_require_tools PATH_NAME TOOL... — say, on stderr, one line per TOOL that
# is not on PATH (a TOOL with `::` is a perl module perl cannot load), and
# return nonzero if any is missing: a script checks the tools it runs before
# first use and shows this line where its caller sees it.
sot_require_tools() {
    local path_name="$1" t rc=0
    shift
    for t in "$@"; do
        case "$t" in
            *::*) perl -M"$t" -e 1 >/dev/null 2>&1 && continue; t="perl's $t" ;;
            *) command -v "$t" >/dev/null 2>&1 && continue ;;
        esac
        printf 'sot-comm: cannot %s: %s is missing (install it)\n' "$path_name" "$t" >&2
        rc=1
    done
    return "$rc"
}

# sot_bounded SECS CMD [ARG...] — runs the program CMD (not a shell function) with this shell's stdin, stdout and
# stderr, and ends it SECS seconds (a whole number above 0) after it starts.
# The bounded comm calls run under it: sot_ssh_bridge's ssh, sot_dial's bridge,
# comm-list.sh's `sot-fe version`, comm-turn-auditor.sh's headless claude, and
# the PostToolUse heartbeat's comm-context.sh, and agents/sot-gh-auth.sh's gh calls. scripts/install.sh carries a copy for its ssh check, which runs before
# the checkout holding this library exists; rust/backend/tests/shell_dial.rs runs every case against both and pins the
# two bodies byte-identical.
# One perl process owns the deadline and CMD's process group, which CMD gets before it runs, or it does not
# run. The call returns when CMD has exited and no member of its group is left; until then the deadline holds, so a
# descendant still holding CMD's output after CMD exits is ended at the bound too. At the bound, or when that perl is
# itself sent TERM, INT or HUP, it signals the group and CMD itself (TERM, or the signal it got), so CMD is reached
# even if it left its group; a second later by the clock it KILLs what is left, and a second after that it returns in
# any case, naming on stderr whatever still runs; it also names CMD's own status when CMD had exited before its group
# was ended. Status: CMD's own when CMD and its group end by themselves; 124 at the bound, 137 when that took KILL;
# 128+N when the bound itself was sent signal N; 127 when CMD could not start; 125 when nothing ran (no perl, no
# process group, or a bound that is not a whole number above 0). Outside it: a descendant that leaves CMD's group
# (setsid, as ssh's ControlPersist master does). On Windows, Git Bash emulates the group and its signals for its own
# programs; whether the bound ends a native Windows program (sot_dial's sotd.exe, the auditor's claude) or its
# children is not established. Not GNU timeout: it returns once its own child ends, leaving a TERM-ignoring
# descendant holding the output, and macOS has none. Only CMD's own process asks for its group, and it trusts the
# group it is then in, not the call's status: on macOS 26 a setpgid by the parent for the same child, at the same
# moment, made either call fail with EPERM though the group was made, and CMD never ran (hosted run 39218378845).
sot_bounded() {
    command -v perl >/dev/null 2>&1 || {
        echo "sot_bounded: no perl to bound $2 with" >&2
        return 125
    }
    perl -e '
        use POSIX ();
        my ($secs, @cmd) = @ARGV;
        if ($secs !~ /^[1-9][0-9]*$/) { print STDERR "sot_bounded: the bound is a whole number of seconds above 0, not $secs\n"; exit 125; }
        my %num = (TERM => POSIX::SIGTERM(), INT => POSIX::SIGINT(), HUP => POSIX::SIGHUP());
        my $pid = fork;
        if (!defined $pid) { print STDERR "sot_bounded: fork: $!\n"; exit 125; }
        if (!$pid) {
            if (!setpgrp(0, 0) && getpgrp() != $$) { print STDERR "sot_bounded: no process group for $cmd[0]: $!\n"; POSIX::_exit(125); }
            exec { $cmd[0] } @cmd;
            print STDERR "sot_bounded: $cmd[0]: $!\n";
            POSIX::_exit(127);
        }
        my ($why, $late, $st) = ("", 0, undef);
        $SIG{ALRM} = sub { $late = 1 };
        $SIG{$_} = sub { $why ||= $_[0] } for qw(TERM INT HUP);
        my $code = sub { $_[0] & 127 ? 128 + ($_[0] & 127) : $_[0] >> 8 };
        my $left = sub {
            $st = $? if !defined $st && waitpid($pid, POSIX::WNOHANG()) == $pid;
            !defined $st || kill(0, -$pid);
        };
        my $wait = sub {
            $late = 0;
            alarm 1;
            select(undef, undef, undef, 0.01) while $left->() && !$late;
            alarm 0;
            $left->();
        };
        alarm $secs;
        select(undef, undef, undef, 0.01) while $left->() && !$why && !$late;
        alarm 0;
        exit($code->($st)) if !$left->();
        $SIG{$_} = "IGNORE" for qw(TERM INT HUP);
        my $own = $st;
        my $end = sub { kill($_[0], -$pid); kill($_[0], $pid) if !defined $st };
        $end->($why || "TERM");
        my $hard = $wait->();
        if ($hard) {
            $end->("KILL");
            print STDERR "sot_bounded: $cmd[0] or a member of its group was still running a second after KILL\n" if $wait->();
        }
        print STDERR "sot_bounded: $cmd[0] exited with status " . $code->($own) . " while its group ran on; the bound ended the group\n" if defined $own;
        exit($why ? 128 + $num{$why} : $hard ? 137 : 124);
    ' "$@"
}

# sot_jq ARGS... — run jq, but normalise ITS OWN OUTPUT so a caller that
# captures a single field (command substitution) or splits several
# records (mapfile / `while read`) never keeps a stray carriage return
# glued to the value. On Windows, a native (non-MSYS) `jq.exe` opens
# stdout in text mode and rewrites every \n it writes to \r\n; bash's own
# consumption idioms keep that \r — command substitution strips only the
# final \n, and mapfile/`read` split on \n only — so a handle, host,
# workspace id or registry root read back this way never compares equal
# to the clean string it should match (field report, 2026-09-27: a
# handle read back as "alpha-host\r" made every send from that box
# report "no such handle" for a delivery that had already landed). Exit
# status is jq's OWN, captured before the cleanup pipe runs, so this is a
# drop-in replacement for `jq` even in a boolean `-e` test.
#
# Use this ONLY where the extracted value is an IDENTIFIER — a handle, a
# host, a workspace id, a protocol version, a phase/state tag, a cursor
# offset, a filename component, or anything else fed into a comparison.
# A field that is free-text CONTENT (a message body, a status summary a
# human wrote) keeps calling `jq` directly: the same text-mode rewrite can
# add a \r before a newline the sender typed on purpose, and stripping it
# there would silently rewrite what they wrote instead of fixing a
# comparison.
sot_jq() {
    # STREAMED, not captured: an earlier body took jq's output through a
    # command substitution, which strips every trailing newline, so a
    # `while read` consumer silently lost its LAST record on every platform --
    # comm-list.sh printed 14 of 15 registered agents. Streaming also means jq
    # can now see a downstream close, so no caller of this ends its pipeline in
    # `head`: the filter picks the one value it wants instead.
    command jq "$@" | tr -d '\r'
    return "${PIPESTATUS[0]}"
}

# --- free text and jq ---
#
# When a NATIVE jq.exe runs under git-bash, MSYS2 rewrites every argv element that starts with "/" into a Windows
# path before jq sees it (field-measured: only the first character of the argument matters; "./", "~/", "//server"
# and a slash inside the value are untouched; on a multi-line value only the first line changes). A message body, a
# path, a project root, a label, a caption, code, findings and base64 can each start with "/", so none of them is
# ever a `--arg` value: free text reaches jq on stdin. One value is jq's raw input,
# `printf '%s' "$v" | jq -Rs ...` with the value as `.`. Several are a stream of JSON texts (`sot_json_escape` for
# each free-text value, an object already built as itself) read with `jq -n` and `input`, one `input` per value in
# stream order, so a value missing from the stream is jq's error, never a null or the next value in its place. Never
# a temporary file: a file outlives a caller that fails or is killed before its `rm`.
# --- cleanup traps ---
#
# A cleanup trap is an EXIT trap that removes or undoes what its shell holds. Its body starts `trap "" HUP INT TERM`,
# so nothing it runs can be ended by a later signal (SIGKILL still can), and ends with sot_end_by_signal. Right after
# setting it, its shell runs sot_cleanup_signals, a subshell too (it keeps none of its parent's traps), and the names
# the trap reads are not `local` (in a command substitution, bash 3.2's `exit` drops a function's locals before the
# trap runs). Left to bash, which runs an EXIT trap when one of the three ends a shell, the trap is lost when a second
# signal comes within a millisecond, as it often does (a shell that dies of a hang-up hangs up its jobs, which without
# job control are its own group, and so does a terminal's shell when the terminal closes): bash 5.2.14 and later
# (patch bash52-014) end the shell at a second HUP or TERM, before the trap's first command, and bash 3.2 never runs a
# subshell's EXIT trap on HUP or TERM once its parent has set one; 5.0 and 5.1 finish it. A trapped signal acts when
# the foreground command ends: a signal to the group ends that command too, one to the shell alone waits for it.
# The shell is armed before it holds anything: `trap - HUP INT TERM`, which this library runs when it loads (above)
# and a shell that does not load it runs itself. bash learns a signal's disposition the first time a trap command
# names it, by setting the signal to its default for an instant, and a signal in that instant ends the shell before
# its trap removes anything. Arming resets the shell's own traps on the three, so a shell traps them after it arms,
# and nothing a cleanup trap's body runs in its own shell arms again. Left as they are: a second signal in the
# microseconds before a handler's first command (bash drops it, and may warn `run_pending_traps: bad value`), SIGKILL.
# sot_cleanup_signals: HUP, INT and TERM each ignore the three, note the signal, turn errexit off (a failing command
# in the trap's body would end the trap before sot_end_by_signal) and exit with 128 plus its number; the note starts
# empty (a subshell's copy of one is not its own). sot_end_by_signal: a shell one of them ended then ends
# by it, as an `sh` that signals itself (in a subshell `$$` names the script; bash 3.2 has no `$BASHPID`), so its
# caller sees the signal, and a bash that waits on it stops on a Ctrl-C (it goes on when that exits normally).
sot_cleanup_signals() {
    local s; _SOT_SIG=""
    for s in HUP:129 INT:130 TERM:143; do trap "trap '' HUP INT TERM; _SOT_SIG=${s%:*}; set +e; exit ${s#*:}" "${s%:*}"; done
}
sot_end_by_signal() { [ -z "${_SOT_SIG:-}" ] || { trap - "$_SOT_SIG"; exec sh -c 'kill -s "$1" "$$"' sh "$_SOT_SIG"; }; }

# sot_host — this shell's DECLARED host name for the wire only (ADR 0046
# decision 1, S1/S2): the ONE resolver matching
# sot_platform::host::state_dir::host_name() on the Rust side exactly — `$SOT_SELF_HOST`
# verbatim if set and non-empty (a NEW variable: `SOT_HOST` already means
# the SSH target a remote frontend dials, `scripts/launch-sot.ps1`/
# `launch-sot.sh` — reusing it here would silently rename a frontend's
# declared identity to whatever it dials), else the first `.`-label of
# `hostname -s`, lowercased. Feeds `sot_hello_frame`'s wire `host` field,
# display/logs, and comm-context.sh's `HOST`, which keys the registry row and
# the unpinned self slot.
# The declared host owns registry facts and self-slot keys; raw host text is only the derived-handle component and validated legacy-slot input. Local defaults use HANDLE_HOST; remote defaults use the admitted target's declared host.
# Fails loudly (S19) rather than printing empty when neither source
# resolves — a hello with an empty declared host is worse than a hello
# that never sent one at all.
sot_host() {
    if [ -n "${SOT_SELF_HOST:-}" ]; then
        printf '%s\n' "$SOT_SELF_HOST"
        return 0
    fi
    local raw
    if ! raw="$(hostname -s 2>/dev/null || hostname 2>/dev/null)"; then
        echo "sot_host: no SOT_SELF_HOST override and hostname failed -- cannot declare an identity" >&2
        return 1
    fi
    raw="${raw%%.*}"
    # Trim whitespace the same way Rust's host_name() does (`.trim()`
    # after taking the first label) — round 2: the two
    # implementations must apply the SAME rule, not two independently
    # coded near-matches.
    raw="${raw#"${raw%%[![:space:]]*}"}"
    raw="${raw%"${raw##*[![:space:]]}"}"
    if [ -z "$raw" ]; then
        echo "sot_host: no SOT_SELF_HOST override and hostname returned no usable label" >&2
        return 1
    fi
    printf '%s\n' "$raw" | tr '[:upper:]' '[:lower:]'
}

# fmt_age SECONDS — compact relative age ("just now"/"2m ago"/"1h ago"/"3d
# ago"). Moved here from comm-list.sh (session-listing brief) so the same
# ageing rule serves every state-nav printer instead of two copies drifting:
# comm-list.sh's own agent rows and sot-fe's `version` command, which now
# prints the same "[state] summary · age" shape for a declared `fe.sessions`
# row (ADR: `status_at` is the honesty valve — an hour-old stamp prints
# "1h ago" wherever it's shown, local row or declared one alike).
fmt_age() {
    local s="$1"
    if   [ "$s" -lt 60 ];    then echo "just now"
    elif [ "$s" -lt 3600 ];  then echo "$((s / 60))m ago"
    elif [ "$s" -lt 86400 ]; then echo "$((s / 3600))h ago"
    else                          echo "$((s / 86400))d ago"
    fi
}
