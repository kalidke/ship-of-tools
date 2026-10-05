# comm-lib-inbox.sh: the inbox append and its lock, the read cursor and the line counts.
# Sourced by comm-lib.sh; defines functions and globals only.

# --- the inbox append (0031 B1) ---
# sot_inbox_append HANDLE — THE one place a script appends a frame to an
# inbox. One JSON line on stdin. 0 = filed; 1 = nothing appended, with the
# reason on stdout for the caller to print after `FAILED -> @h: `.
#
# The lock is the kernel's file lock (flock) on the sidecar
# `inbox/<handle>.lock`, which the daemon's `comm.file` filer takes too. The
# OS releases it when its holder dies, so there is no reclaim; a frozen holder
# only makes the next sender wait, and a sender that cannot take the lock
# within SOT_INBOX_LOCK_WAIT_SECS appends NOTHING and fails — a `filed` for an
# append that did not happen is a false success. The inbox is opened inside
# the lock and closed before it is released: correctness is the lock plus
# close-to-open consistency, never O_APPEND's offset across boxes.
#
# A lock excludes only writers that go through ONE lock manager: an NFSv3 and
# an NFSv4 lock on one export exclude nothing, and a local flock on a disk
# other hosts mount does not exclude their NFS locks. So a script appends
# locally only when flock(1) and perl exist, this is Linux, and the identity it
# computes for $INBOX_DIR is byte-equal to line 1 of
# `$COMM_HOME/inbox-lock-manager`, which only the folder's hub writes, at
# startup, by an exclusive create (line 2 is the writing machine's id). On
# NFSv3 or an unknown mount the identity is `none@<machine-id>`: the folder is
# bound to the one machine that wrote the record, whose processes share its
# one kernel lock, and every other machine computes a different string.
# Anything else — no record, a bare `none` record, another machine's
# `none@…` (NFSv3, an unknown mount, an NFS v4 mount whose `local_lock` is not
# `none`), another host mounting the daemon's local disk — hands the frame to
# the daemon that owns this comm folder as `comm.file`, and a daemon that does
# not answer is FAILED. A daemon makes the same check at each filing: a guest
# on the hub's folder forwards what it cannot prove to the hub, and the hub
# refuses it with the recovery named.
#
# An unterminated tail a dead writer left is CUT back to the last newline under
# the lock (a note on stderr says how many bytes), never ended. Readers count
# newline-terminated lines only, so a cut never moves a cursor, and two guards
# cover a line a failed append then cuts back: where a writer would append
# locally the reader's count-and-read takes a shared lock bounded by
# SOT_INBOX_READ_WAIT_SECS (a lock held past it means try again, exit 75; any
# other lock fault, a lock file that will not open or any flock error, is named
# and the read runs unlocked) — comm-poll lets go before it
# shows anything — and on every host the cursor keeps
# `<count> <crc>-<len>` of the last line the reader READ (hashed from the bytes
# it holds, never re-read from the file), so a reader steps back one line when
# a cut-back removed it.
SOT_INBOX_LOCK_WAIT_SECS="${SOT_INBOX_LOCK_WAIT_SECS:-10}"
_sot_have_flock() { command -v flock >/dev/null 2>&1; }
_sot_findmnt() { findmnt "$@"; }
_sot_machine_id() { local m=""; { read -r m < /etc/machine-id; } 2>/dev/null; printf '%s' "$m"; }
# sot_inbox_lock_identity DIR — the lock manager an append to DIR goes
# through, by the rule comm/mail/inbox.rs's record uses: `nfs4 <source>`,
# `local <machine-id>` on a local block filesystem, else `none@<machine-id>`,
# this machine's lock alone (bare `none` with no machine id, which never
# matches) — so an NFS v4 mount without `local_lock=none` (its lock stays on
# the client) is local only on the machine that wrote the record. The last
# line of `findmnt -T` is the mount on top when one is stacked over another.
sot_inbox_lock_identity() {  # DIR
    local fs="" opts="" src="" mid
    read -r fs opts src < <(_sot_findmnt -n -o FSTYPE,FS-OPTIONS,SOURCE -T "$1" 2>/dev/null | tail -n 1)
    case "$fs" in
        nfs4) case ",$opts," in
                  *,vers=4.*,*) case ",$opts," in
                      *,local_lock=none,*) [ -z "$src" ] || { printf 'nfs4 %s\n' "$src"; return 0; } ;;
                  esac ;;
              esac ;;
        ext2|ext3|ext4|xfs|btrfs|zfs|f2fs)
            mid="$(_sot_machine_id)"
            [ -z "$mid" ] || { printf 'local %s\n' "$mid"; return 0; } ;;
    esac
    mid="$(_sot_machine_id)"
    [ -z "$mid" ] || { printf 'none@%s\n' "$mid"; return 0; }
    printf 'none\n'
}
# _sot_inbox_lock_ours DIR — prints this script's sot_inbox_lock_identity for
# DIR when it is the lock manager every other writer of the inbox takes (the
# record's line 1), and nothing otherwise: no flock(1), no perl, not Linux, or
# a missing, empty or bare `none` record. The cheap tests come first, so a box
# that can never take the lock runs no findmnt.
_sot_inbox_lock_ours() {  # DIR
    local rec="" id
    _sot_have_flock && command -v perl >/dev/null 2>&1 && [ "$(uname -s 2>/dev/null)" = Linux ] || return 0
    { IFS= read -r rec < "$COMM_HOME/inbox-lock-manager"; } 2>/dev/null
    [ -n "$rec" ] && [ "$rec" != none ] || return 0
    id="$(sot_inbox_lock_identity "$1")"
    [ "$id" != "$rec" ] || printf '%s\n' "$id"
}
# _sot_flock_wait MODE SECS ID — the lock on fd 9 (MODE -x or -s) within SECS,
# chosen by ID, the identity _sot_inbox_lock_ours printed.
# The Linux NFSv4 client retries a blocked lock with a backoff that doubles
# from 100 ms, so a local writer re-takes the lock before a remote waiter's
# next retry and a blocking waiter can sleep past a free lock: under `nfs4 ` a
# non-blocking try is repeated every 15-25 ms until the bound. NLM (v3) and one machine's own
# kernel lock (`local …`, `none@…`) wake a blocked waiter on release, so those
# block, bounded. 75 = the bound passed with the lock held elsewhere; any
# other non-zero is flock's own error, never a held lock, and flock names it on
# stderr (`flock: 9: <strerror>`).
_sot_flock_wait() {  # MODE SECS ID
    local rc end
    case "$3" in
        "nfs4 "*)
            end=$(( $(date +%s%N) + $2 * 1000000000 ))
            while :; do
                rc=0
                flock -n -E 75 "$1" 9 || rc=$?
                [ "$rc" -eq 75 ] || return "$rc"
                [ "$(date +%s%N)" -lt "$end" ] || return 75
                sleep "0.0$((15 + RANDOM % 11))"
            done ;;
        *) flock "$1" -w "$2" -E 75 9 ;;
    esac
}
# _sot_append_whole FILE LINE — under the caller's lock, LINE goes in whole or
# not at all, in ONE perl process on ONE descriptor: the length before is a
# seek to its end (a path stat can be answered from the attribute cache and
# cut away a line another host filed).
# An unterminated tail (a writer that died mid-line, or NULs after a client
# crash) is CUT back to the last newline first, in blocks on that descriptor,
# and the cut is noted on stderr: everything past the last newline was
# written by a writer that never answered `filed`, so nothing kept is lost.
# 0 means written, fsynced and closed, and any failure cuts FILE back to the
# length after that cut, on the same descriptor. LINE goes on stdin, never
# argv.
_sot_append_whole() {  # FILE LINE
    printf '%s\n' "$2" | perl -e '
        use strict; use Fcntl qw(O_RDWR O_APPEND O_CREAT SEEK_SET SEEK_END); use IO::Handle;
        my $f = shift; my $buf = do { local $/; <STDIN> }; my ($fh, $len);
        sub fail { my $e = "$!"; truncate($fh, $len) if defined $len; print STDERR "$f: $e\n"; exit 1 }
        sub readat { my ($at, $n) = @_; my $got = "";
            defined sysseek($fh, $at, SEEK_SET) or fail();
            while (length($got) < $n) { my $r = sysread($fh, $got, $n - length($got), length($got)) // fail(); $r > 0 or fail() }
            $got }
        sysopen($fh, $f, O_RDWR | O_APPEND | O_CREAT, 0666) or fail();
        defined($len = sysseek($fh, 0, SEEK_END)) or fail();
        $len += 0;   # sysseek says "0 but true" for 0
        if ($len > 0 && readat($len - 1, 1) ne "\n") {
            my ($end, $cut) = ($len, 0);
            while ($end > 0) {
                my $start = $end > 4096 ? $end - 4096 : 0;
                my $i = rindex(readat($start, $end - $start), "\n");
                if ($i >= 0) { $cut = $start + $i + 1; last }
                $end = $start;
            }
            truncate($fh, $cut) or fail();
            my ($h) = $f =~ m{([^/]*)\.jsonl$};
            print STDERR "note: cut " . ($len - $cut) . " bytes of an unterminated line a dead writer left in \@$h\x27s inbox\n";
            $len = $cut;
        }
        for (my $off = 0; $off < length $buf;) { $off += syswrite($fh, $buf, length($buf) - $off, $off) // fail() }
        $fh->sync or fail();
        close($fh) or fail();
    ' "$1"
}
# A reader that polls after a writer's `write` but before its `fsync` fails
# would count a line the writer then cuts back, and its line-count cursor would
# sit one past the end and skip the next message. Where a writer would append
# locally (the test above) a reader therefore counts and reads under a SHARED
# lock on the writers' `inbox/<h>.lock`, waiting at most
# SOT_INBOX_READ_WAIT_SECS. The lock descriptor is opened read-write everywhere
# (`9<>`): the Linux NFS client refuses a shared lock on one without read
# access. sot_inbox_read_lock takes it on fd 9 of the calling
# shell (never a subshell: the reader's counters must survive) and returns 0
# when held or when no lock applies, 75 when the bound passed with the lock
# held elsewhere — which means try again, never a skip. Any other lock fault
# is not "try again": a lock file that will not open, or any other flock error,
# returns 0 unheld, with SOT_INBOX_READ_WARNING naming it (flock's code and its
# own stderr text) for the caller to show where its session sees it. flock runs
# inside $(…) to catch that text: the lock is on the open file description,
# which the calling shell's fd 9 still holds. An unheld reader, like every
# reader elsewhere, is covered by the cursor's line hash (sot_cursor_write): a
# line may show twice, none is lost. comm-poll reads its batch under the lock,
# lets go, then shows it: a slow display never holds off a writer.
SOT_INBOX_READ_WAIT_SECS="${SOT_INBOX_READ_WAIT_SECS:-3}"
sot_inbox_read_lock() {  # HANDLE
    local rc=0 id err
    SOT_INBOX_READ_WARNING=""
    id="$(_sot_inbox_lock_ours "$COMM_HOME/inbox")"
    [ -n "$id" ] || return 0
    if ! { exec 9<> "$COMM_HOME/inbox/$1.lock"; } 2>/dev/null; then
        SOT_INBOX_READ_WARNING="WARNING: the inbox lock for @$1 failed (cannot open its lock file) — reading without it; a line may show twice, none is lost"
        return 0
    fi
    err="$(_sot_flock_wait -s "$SOT_INBOX_READ_WAIT_SECS" "$id" 2>&1)" || rc=$?
    [ "$rc" -ne 0 ] || return 0
    exec 9>&-
    [ "$rc" -ne 75 ] || return 75
    err="${err##*$'\n'}"
    [ -n "$err" ] && err="$rc: ${err##*: }" || err="code $rc"
    SOT_INBOX_READ_WARNING="WARNING: the inbox lock for @$1 failed ($err) — reading without it; a line may show twice, none is lost"
}
sot_inbox_read_unlock() { exec 9>&-; }
sot_inbox_append() {  # HANDLE
    local h="$1" line err rc=0 id
    line="$(cat)"
    id="$(_sot_inbox_lock_ours "$INBOX_DIR")"
    if [ -z "$id" ]; then
        _sot_inbox_append_via_daemon "$h" "$line"
        return
    fi
    # 75 is flock's own conflict exit (-E), so a lock that was never taken is
    # told apart from an append that failed under it.
    err="$( { ( _sot_flock_wait -x "$SOT_INBOX_LOCK_WAIT_SECS" "$id" || exit $?
                _sot_append_whole "$INBOX_DIR/$h.jsonl" "$line"
              ) 9<> "$INBOX_DIR/$h.lock"; } 2>&1 )" || rc=$?
    case "$rc" in
        0)  [ -z "$err" ] || printf '%s\n' "$err" >&2   # the cut's note
            return 0 ;;
        75) printf 'the inbox lock for @%s was held for %ss — nothing was appended\n' \
                "$h" "$SOT_INBOX_LOCK_WAIT_SECS" ;;
        *)  printf 'the append failed: %s\n' "${err##*$'\n'}" ;;   # the last line: a cut's note may precede it
    esac
    return 1
}
# The daemon that owns this comm folder: this box's own when there is one,
# else the relay endpoint (the hub, for a shared-home box that runs no
# daemon). One route, chosen once: an endpoint that does not answer is FAILED.
_sot_inbox_append_via_daemon() {  # HANDLE LINE
    local ENDPOINT
    ENDPOINT="$(sot_daemon_endpoint 2>/dev/null)" || ENDPOINT=""
    [ -n "$ENDPOINT" ] || { ENDPOINT="$(sot_relay_endpoint 2>/dev/null)" || ENDPOINT=""; }
    if [ -z "$ENDPOINT" ]; then
        printf 'this box cannot take the inbox lock itself, and no daemon is reachable to file it\n'
        return 1
    fi
    sot_comm_file "$1" "$2" || return 1
}

# sot_comm_file HANDLE LINE — THE one `comm.file` request and its verdict, for
# the guard's daemon route above and comm-relay.sh's send_frame alike. LINE is
# the inbox line (`from`, `to`, `msg`); ENDPOINT comes from the caller's scope.
# 0 = filed; otherwise the reason is on stdout for the caller to print after
# `FAILED -> @h: `, and the status is 2 when the daemon does not list HANDLE
# (`not_here`), 1 for everything else. The response line decides, in this
# order: an `error` is FAILED in the daemon's own words (keyed on its
# presence, never on `code` — an older daemon refuses the unknown op with no
# code); `ok` is filed whatever the transport's exit status or stderr say; no
# response is FAILED, and only then does the transport's stderr give the why.
sot_comm_file() {  # HANDLE LINE
    local h="$1" frame resp reason code err diag window
    # The daemon may wait the whole inbox-lock bound before it files, so a read
    # window no longer than that reports FAILED for a line that WAS filed, and
    # the sender resends it: the lock wait plus 10s for the transport's setup.
    window=$(( SOT_INBOX_LOCK_WAIT_SECS + 10 ))
    [ "${SOT_SEND_TIMEOUT:-0}" -gt "$window" ] 2>/dev/null && window="$SOT_SEND_TIMEOUT"
    # The text reaches jq on stdin, never argv (the MSYS2 guard, sot_jq_rawfile).
    # A broadcast copy (the line's own `to` empty) must stay one after filing.
    frame="$(printf '%s' "$2" | jq -c --arg t "$h" \
        '{v:1,id:1,kind:"req",op:"comm.file",payload:{from:.from,to:$t,text:.msg,broadcast:(.to == "")}}')" || {
        printf 'the frame could not be built\n'; return 1; }
    err="$(mktemp "${XDG_RUNTIME_DIR:-/tmp}/sot-comm-file-XXXXXX")" || err=""
    resp="$(SOT_SEND_TIMEOUT="$window" sot_oneshot_request "$frame" comm.file 2>"${err:-/dev/null}")" || resp=""
    diag=""
    if [ -n "$err" ]; then diag="$(tr '\n' ' ' < "$err")"; rm -f "${err:?}"; fi
    reason="$(printf '%s' "$resp" | sot_jq -r '.payload.error // empty' 2>/dev/null)" || reason=""
    if [ -n "$reason" ]; then
        printf '%s\n' "$reason"
        code="$(printf '%s' "$resp" | sot_jq -r '.payload.code // empty' 2>/dev/null)" || code=""
        [ "$code" = not_here ] && return 2
        return 1
    fi
    # `-n` first: `jq -e` over empty input exits 0.
    if [ -n "$resp" ] && printf '%s' "$resp" | jq -e '.payload.ok == true' >/dev/null 2>&1; then
        return 0
    fi
    diag="${diag% }"
    printf 'the daemon did not answer at %s%s\n' "$ENDPOINT" "${diag:+: $diag}"
    return 1
}

# --- the read cursor: a LINE OFFSET into inbox/<handle>.jsonl ----------------
#
# read/<handle>.cursor holds the NUMBER of inbox lines the recipient has been
# shown. It used to hold the newest-shown `ts`, and those stamps are
# second-resolution while every comparison was strictly-greater: a frame filed
# in the same second as one already read was never shown and never announced,
# while the sender printed a success line — a false-positive acknowledgement,
# the one thing this design may not produce. A count cannot lose a frame that
# way.
#
# sot_cursor_offset HANDLE — that offset. Three rules, each one a way this could
# otherwise go silently deaf:
#
#   * A LEGACY ts cursor is converted in memory (never written here — only a real
#     comm-poll.sh advances the cursor) by counting the lines BEFORE THE FIRST
#     one whose ts is greater than it. Not "every line at or below it": stamps
#     are only in order if every sender's clock agrees, and with a skewed clock
#     across hosts (or two frames in one second) that count would step PAST an
#     unread frame already on disk and it would never be shown. Stopping at the
#     first greater line inherits no loss at all.
#   * An unparseable line counts as read and never as a boundary. A torn append
#     is realistic on a shared filesystem, and one must not be able to freeze
#     the cursor: that makes a handle permanently deaf while its senders keep
#     printing a success line.
#   * An offset PAST the end of the inbox is 0. Production only appends, so this
#     means the file was cleared, truncated or restored by hand — exactly the
#     moment nobody suspects the cursor, and left as-is the handle never sees
#     another message. The one exception: a hashed cursor exactly one past the
#     end is the last line read, cut back with nothing filed since — one step
#     back, like any other cut-back.
#
# Anything unreadable yields 0. On doubt this biases LOW: showing a frame twice
# is tolerable where dropping one is not. One exception: the timestamp form
# needs jq, and a box without it would read 0 -- the whole inbox as unread, or
# for a counter a false "nothing" -- so it says so and returns 1, printing no
# offset.
sot_cursor_offset() {
    local handle="$1" cur n cnt hash="" total
    # $COMM_HOME, not the source-time $READ_DIR: a script may re-derive its
    # home inside its own main, and a helper reading a different one than its
    # caller is a silently wrong answer.
    cur="$(cat "$COMM_HOME/read/$handle.cursor" 2>/dev/null || true)"
    [ -n "$cur" ] || { printf '0\n'; return 0; }
    # `<count>` (every cursor written before the hash existed) or
    # `<count> <hash>`; anything else is the legacy ts form.
    cnt="${cur%% *}"
    [ "$cnt" = "$cur" ] || hash="${cur#* }"
    case "$cnt" in
        ''|*[!0-9]*) ;;
        *)
            total="$(sot_inbox_lines "$handle")"
            if [ "$cnt" -gt "$total" ]; then
                if [ "$cnt" -eq $((total + 1)) ] && [ -n "$hash" ]; then
                    printf "note: the last line read from @%s's inbox was cut back; reading from the line before it\n" "$handle" >&2
                    printf '%s\n' "$total"
                else
                    printf '0\n'
                fi
                return 0
            fi
            # A hash says which line the cursor consumed last. If line CNT is
            # no longer it, a cut-back removed it (the append that wrote it
            # failed after a reader counted it), and a cut-back removes at most
            # that one line: one step back is exact.
            if [ "$cnt" -gt 0 ] && [ -n "$hash" ] \
                && [ "$(sot_line_hash "$COMM_HOME/inbox/$handle.jsonl" "$cnt")" != "$hash" ]; then
                printf "note: the last line read from @%s's inbox was cut back; reading from the line before it\n" "$handle" >&2
                cnt=$((cnt - 1))
            fi
            printf '%s\n' "$cnt"; return 0 ;;
    esac
    sot_require_tools "read the timestamp cursor of @$handle" jq || return 1
    n="$(sot_jq -Rrs --arg cur "$cur" '
        [ split("\n")[] | select(length > 0)
          | ((((fromjson? | objects) // {}) | (.ts // "")) > $cur) ] as $past
        | ($past | index(true)) // ($past | length)' \
        "$COMM_HOME/inbox/$handle.jsonl" 2>/dev/null)" || n=0
    [[ "$n" =~ ^[0-9]+$ ]] || n=0
    _sot_clamp_offset "$handle" "$n"
}

# _sot_hash_stdin — THE line hash: `<crc>-<len>` (cksum, POSIX, in git-bash
# too) of stdin without its newlines and NULs. A NUL is dropped because bash
# drops it from a reader's copy, so the file's bytes and the copy hash alike.
_sot_hash_stdin() { tr -d '\n\000' | cksum | awk '{print $1 "-" $2}'; }

# sot_line_hash FILE N — the line hash of line N of FILE.
sot_line_hash() {
    sed -n "${2}p" "$1" 2>/dev/null | _sot_hash_stdin
}

# sot_cursor_write HANDLE COUNT LINE — the read cursor: `<count> <hash of
# LINE>`, or just `0`. LINE is the bytes of line COUNT as the caller read them
# (no newline), never re-read from the file: a line shown and then cut back
# must not have its hash taken from the line filed in its place. The hash is
# _sot_hash_stdin's, the one sot_line_hash takes of the file, so a line holding
# a NUL (bash has already dropped it from LINE) hashes the same on both sides.
sot_cursor_write() {
    local h="$1" n="$2"
    if [ "$n" -gt 0 ]; then
        printf '%s %s' "$n" "$(printf '%s' "$3" | _sot_hash_stdin)" > "$COMM_HOME/read/$h.cursor"
    else
        printf '0' > "$COMM_HOME/read/$h.cursor"
    fi
}

# _sot_clamp_offset HANDLE N — N, or 0 when it points past the end of the inbox.
_sot_clamp_offset() {
    local total; total="$(sot_inbox_lines "$1")"
    if [ "$2" -gt "$total" ]; then printf '0\n'; else printf '%s\n' "$2"; fi
}

# sot_file_lines PATH — PATH's line count, 0 when it is absent or unreadable.
# The invariant every inbox reader leans on: this counts newline-TERMINATED
# lines only (`wc -l`), and readers read `sed -n "a,${count}p"` up to that
# count, so an unterminated tail a dead writer left is never counted, never
# read, and its later cut never moves a cursor.
# THE line counter: every inbox reader needs one, and each copy was a chance
# to get the two quiet parts wrong. Readability is tested FIRST because the
# SHELL, not wc, prints "No such file" for `< missing` — before wc's own
# 2>/dev/null can suppress it, into whatever the caller's stderr happens to be
# (a durable log, a bootstrap's one-line-per-outcome contract). And the
# count is stripped of the leading spaces a BSD `wc` pads it with, so callers
# can compare it as a number without each one remembering to.
sot_file_lines() {
    local n=""
    [ -r "$1" ] && n="$(wc -l < "$1" 2>/dev/null | tr -d ' ')"
    [[ "$n" =~ ^[0-9]+$ ]] || n=0
    printf '%s\n' "$n"
}

# sot_inbox_lines HANDLE — the inbox's line count (0 when absent).
sot_inbox_lines() {
    sot_file_lines "$COMM_HOME/inbox/$1.jsonl"
}

# sot_unread HANDLE — THE unread count, the shell twin of the wake's `counts`
# (rust/backend/src/comm/wake/unread.rs; `unread_agrees_with_the_shell` pins
# them). Prints one line, `<total> <unread>`, as its last output: the inbox's
# line count and how many lines past the read cursor are JSON objects whose
# `to` is a string equal to HANDLE and whose `from` is not HANDLE. A line
# addressed to another handle, broadcast (`to` ""), or with no string `to`
# never counts, and neither does a line holding a NUL (not JSON to the wake). Takes no lock: the caller holds the read lock. A count that
# cannot be made prints nothing on stdout and returns 1 with the reason on
# stderr, never 0: without jq, or when jq fails.
sot_unread() {
    local h="$1" pos total n=0
    sot_require_tools "count unread mail for @$h" jq || return 1
    pos="$(sot_cursor_offset "$h" 2>/dev/null)" || return 1
    total="$(sot_inbox_lines "$h")"
    if [ "$total" -gt "$pos" ]; then
        n="$(sed -n "$((pos + 1)),${total}p" "$COMM_HOME/inbox/$h.jsonl" 2>/dev/null \
            | sot_jq -Rrs --arg me "$h" '[ split("\n")[] | select(length > 0)
                | select((explode | index(0)) == null)
                | (fromjson? // empty) | select(type == "object")
                | select((.to | type) == "string" and .to == $me and (.from // "") != $me)
              ] | length' 2>&1)"
        [[ "$n" =~ ^[0-9]+$ ]] || {
            printf 'sot-comm: cannot count unread mail for @%s: %s\n' "$h" "${n##*$'\n'}" >&2
            return 1; }
    fi
    printf '%s %s\n' "$total" "$n"
}
