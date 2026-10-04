# comm-lib-registry-lock.sh: the registry lock: with_lock and the lock record's take, judge and fail steps.
# Sourced by comm-lib.sh; defines functions and globals only.

# with_lock CMD [ARGS...] — run CMD holding the registry lock. CMD may be a
# shell function defined in this sourced lib.
#
# The lock is the FILE $COMM_HOME/.registry.lock, one line naming its holder
# (B1b): `name:machine:boot:pidns:pid:start` — the holder's host name,
# /etc/machine-id, the kernel's boot_id, its pid namespace, its pid and that
# pid's start tick (field 22 of /proc/<pid>/stat); a field that cannot be read
# is `-`, which never equals anything. It is made by link(2) of a temp file
# that already holds the line, so it never exists without its holder, and a
# link never replaces anything. The daemon's `comm_registry_lock.rs` takes
# the same lock with the same record.
#
# Proof of death is Linux-only, and only from the holder's own machine: the
# same boot and pid namespace with the pid gone or its start changed, or the
# same machine-id AND host name with another boot. That assumes a machine-id
# is unique to one machine: a cloned image sharing one would read a live
# clone as rebooted. No timeout proves anything, so a live, frozen or
# unprovable holder is never forced (Codex review, PR #148 F2): the waiter
# FAILS CLOSED at the bound, naming the holder and the one-line recovery.
#
# The reclaim (`_sot_lock_step`) runs on the FIRST failed take, before any
# sleep, so a zero-wait heartbeat and a ~1 s touch reach it too: prove the
# holder D dead, take the marker `.registry.lock.reclaim.<D>` (only its
# creator acts; markers are kept forever, but for the daemon's own), settle
# 1 s for D's orphaned children and in-flight calls, re-read the lock fresh,
# and remove it only if it still names D. A removed lock is retaken at once,
# as part of the try that removed it, even past the deadline.
#
# The bound is a deadline, SOT_LOCK_WAIT_SECS, polled every 50 ms: 10 s for
# every ruled write, about 1 s for the best-effort `last_seen` touches of send,
# poll and spawn, and 0 for the heartbeat. The clock is read from the first
# failed take on, so an uncontended take reads none: bash 5's EPOCHREALTIME,
# or perl's Time::HiRes before bash 5 (3.2 has nothing finer than SECONDS),
# both a wall clock, so a clock jump stretches or shortens one wait. No clock
# is FAILED naming it. Where the deadline is checked is comm/PROTOCOL.md's
# Bounds.
#
# Release is TRAP-based, not a plain post-command `rm` (Codex review F2
# second half / F7): a caller's `set -e` aborts the WHOLE SCRIPT the moment
# `"$@"` fails, at that exact statement — skipping every line after it in
# this function, including a plain `rm` written below the call. That
# leaked the lock forever on any callee failure (a corrupt registry.json
# making `registry_put`'s jq fail, for example). An EXIT trap still fires
# on that abort, so the lock comes off either way.
#
# The PRIOR EXIT trap (if any) is saved and restored, not just cleared:
# bash has one EXIT trap per shell, not a stack, and a caller may already
# have its own (e.g. comm-spawn.sh's provisional-row rollback) active
# around a with_lock call — blindly clearing it here would silently
# disarm the caller's cleanup for the rest of the script. This restore now
# runs on EVERY path, including a directly-failing "$@" (Codex review PR
# #148 round 2, finding 4): a bare `"$@"` statement under the caller's
# `set -e` used to abort the WHOLE SCRIPT right there, skipping every line
# below it in this function — the lock still came off (its own release
# trap fired on that abort), but the restore of the CALLER's prior trap
# never ran, silently losing it for the rest of the script. Capturing the
# callee's status via `if "$@"; then :; else rc=$?; fi` — the standard
# idiom for "run this and don't let -e kill us on failure" — means release
# and restore always execute before this function returns, on every path.
SOT_LOCK_WAIT_SECS=10
with_lock() {
    local deadline="" took retook=""
    # Test seam (F10): let a test PROVE a background waiter has reached its
    # first lock attempt, instead of racing it with a sleep. Touched once,
    # right before that attempt; unset (the default) this is a no-op.
    #
    # `touch --`, NOT `: > FILE` (Codex review round 2, finding 6): a bare
    # `>` redirect TRUNCATES whatever already sits at that path — if this
    # var ever leaked into a production environment pointed at a real
    # file, every with_lock call would zero it. `touch` only updates/
    # creates, and unlike `>` it doesn't attempt to OPEN-FOR-WRITE (which
    # would block forever against a FIFO with no reader, right here in the
    # lock's own hot path) — and `|| true` keeps a bad path from tripping
    # this function's own `set -e`-sensitive callers.
    [ -n "${SOT_COMM_TEST_LOCK_BARRIER:-}" ] && { touch -- "$SOT_COMM_TEST_LOCK_BARRIER" 2>/dev/null || true; }
    _sot_lock_self_id
    while :; do
        # Test seam: a slow try, so a test can show the bound is time.
        [ -z "${SOT_COMM_TEST_LOCK_TRY_DELAY:-}" ] || sleep "$SOT_COMM_TEST_LOCK_TRY_DELAY"
        if _sot_lock_take "$_SOT_REG_LOCK"; then break; else took=$?; fi
        if [ "$took" = 2 ]; then
            echo "ERROR: registry lock $_SOT_REG_LOCK cannot be taken: $_SOT_LOCK_WHY" >&2
            return 1
        fi
        # A failed retake: the FAILED line names whoever took it.
        if [ -n "$retook" ]; then
            retook=""
            _sot_lock_fresh "$_SOT_REG_LOCK" && _SOT_LOCK_HOLDER="$_SOT_LOCK_READ"
        fi
        if [ -z "$deadline" ]; then
            _sot_lock_now "$SOT_LOCK_WAIT_SECS" || return 1
            deadline="$_SOT_LOCK_NOW"
        elif _sot_lock_over "$deadline"; then
            return 1
        fi
        if _sot_lock_step "$deadline"; then
            _SOT_LOCK_HOLDER="" _SOT_LOCK_WHO="" _SOT_LOCK_BYHAND="" _SOT_LOCK_GONE="" retook=1
            _SOT_LOCK_WHY="another process took it as soon as a dead holder's lock was removed"
            continue
        fi
        _sot_lock_over "$deadline" && return 1
        sleep "$_SOT_LOCK_NAP"
        _sot_lock_over "$deadline" && return 1
    done
    # Lock acquired — guarantee release via EXIT trap (see header comment),
    # preserving whatever EXIT trap the caller already had.
    local prev_trap rc=0
    prev_trap="$(trap -p EXIT)"
    trap 'rm -f "${_SOT_REG_LOCK:?}" 2>/dev/null || true' EXIT
    if "$@"; then
        :
    else
        rc=$?
    fi
    rm -f "${_SOT_REG_LOCK:?}" 2>/dev/null || true
    if [ -n "$prev_trap" ]; then
        eval "$prev_trap"
    else
        trap - EXIT
    fi
    return $rc
}

# _sot_lock_now SECS — set _SOT_LOCK_NOW to the clock in microseconds, SECS
# (to the microsecond) from now. The clock is chosen by the bash version, never probed:
# bash 5's EPOCHREALTIME with every non-digit deleted (its separator follows
# the locale), else perl's Time::HiRes. A value that is empty or not all
# digits (an unset EPOCHREALTIME is an ordinary, empty variable) is no clock:
# say the FAILED line naming it, and return 1.
_sot_lock_now() {
    local t what i="${1%%.*}" f
    f="${1#"$i"}"; f="${f#.}000000"
    if [ "${BASH_VERSINFO[0]}" -ge 5 ]; then
        t="${EPOCHREALTIME//[!0-9]/}" what="bash's EPOCHREALTIME is unset or not a number"
    else
        t="$(perl -MTime::HiRes=time -e 'printf "%.0f\n", time * 1e6' 2>/dev/null)" what="perl's Time::HiRes is missing (install it)"
    fi
    if [[ "$t" =~ ^[0-9]+$ ]]; then
        _SOT_LOCK_NOW=$((t + 10#${i:-0} * 1000000 + 10#${f:0:6}))
        return 0
    fi
    echo "ERROR: registry lock $_SOT_REG_LOCK is held, and there is no clock to wait by: $what" >&2
    return 1
}

# _sot_lock_over DEADLINE — 0 when the wait ends, its FAILED line said: the
# deadline has passed, or there is no clock. 1 = time is left, and
# _SOT_LOCK_NAP is a sleep of at most 50 ms that ends by the deadline.
_sot_lock_over() {
    local r
    _sot_lock_now 0 || return 0
    r=$((($1 - _SOT_LOCK_NOW) / 1000))
    if [ "$r" -le 0 ]; then
        _sot_lock_fail_text >&2
        return 0
    fi
    [ "$r" -lt 50 ] || r=50
    printf -v _SOT_LOCK_NAP '0.%03d' "$r"
    return 1
}

# _sot_lock_self_id — set _SOT_LOCK_ID to THIS process's holder record, and
# _SOT_LOCK_SELF to `name:machine:boot:pidns` where this shell can prove a
# death (Linux, with a /proc that is its own), else to "". Call it in the
# process that will hold the lock, never as `id=$(_sot_lock_self_id)`: a
# command substitution is a subshell, and its pid dies at once, so every
# waiter on this machine would reclaim a live holder (review S5). bash 3.2
# has no BASHPID, so there the recorded pid is `$$`, which /proc/self then
# contradicts: nothing is proved from that record, and only the FAILED text is
# affected while no script runs two with_lock subshells at once: sibling
# subshells share `$$`, so they would share one ID and one temp file, and the
# take's `-ef` test would read a sibling's link as its own. No caller does.
_sot_lock_self_id() {
    local name machine=- boot=- pidns=- pid="${BASHPID:-$$}" start=- self_pid="" ns=""
    name="$(sot_host 2>/dev/null)" || name=""
    name="${name//[!A-Za-z0-9._-]/_}"
    name="${name:--}"
    _SOT_LOCK_SELF="" _SOT_LOCK_HOLDER="" _SOT_LOCK_WHO="" _SOT_LOCK_BYHAND="" _SOT_LOCK_GONE=""
    _SOT_LOCK_WHY="it was released just now"
    if [ "$(uname -s 2>/dev/null)" = Linux ]; then
        read -r self_pid _ 2>/dev/null </proc/self/stat || true
    fi
    if [ "$self_pid" = "$pid" ]; then
        machine="$(_sot_machine_id)"
        IFS= read -r boot 2>/dev/null </proc/sys/kernel/random/boot_id || true
        ns="$(readlink "/proc/$pid/ns/pid" 2>/dev/null)" || ns=""
        case "$ns" in 'pid:['*']') pidns="${ns#pid:[}"; pidns="${pidns%]}" ;; esac
        if _sot_lock_start "$pid"; then start="$_SOT_LOCK_START"; fi
        machine="${machine:--}"; boot="${boot:--}"; pidns="${pidns:--}"
        _SOT_LOCK_SELF="$name:$machine:$boot:$pidns"
    fi
    _SOT_LOCK_ID="$name:$machine:$boot:$pidns:$pid:$start"
}

# _sot_lock_is_me ID — 0 when ID is this process's, and only where it carries
# proof: without it the ID is `name:-:-:-:pid:-`, and `-` never equals
# anything, so another box's process with the same host name and pid would
# read as mine (review SF1).
_sot_lock_is_me() {
    [ -n "${_SOT_LOCK_SELF:-}" ] && [ "$1" = "$_SOT_LOCK_ID" ]
}

# _sot_lock_start PID — set _SOT_LOCK_START to field 22 of /proc/PID/stat,
# read after the LAST `)` because the command name may hold spaces and
# parentheses (challenge_unix.rs's process_start_ticks, the same parse).
_sot_lock_start() {
    local line="" f=()
    { IFS= read -r line </proc/"$1"/stat; } 2>/dev/null || [ -n "$line" ] || return 1
    read -r -a f <<<"${line##*)}" || true
    [[ "${f[19]:-}" =~ ^[0-9]+$ ]] || return 1
    _SOT_LOCK_START="${f[19]}"
}

# _sot_lock_take TARGET — one attempt to create TARGET holding _SOT_LOCK_ID:
# 0 taken, 1 held (TARGET exists), 2 an error named in _SOT_LOCK_WHY. `link`,
# never `ln`: `ln` into an existing directory, an older peer's mkdir lock,
# makes TARGET/tmp and "succeeds", which gives two holders (review B1). File
# names map ':' to '.', because Windows reads a ':' in a name as a stream
# (review B2); the record itself keeps its colons.
_sot_lock_take() {
    local tmp="$_SOT_REG_LOCK.tmp.${_SOT_LOCK_ID//:/.}" err taken=""
    # An earlier temp is removed before this one is written: one a take could
    # not remove may still be a link to that take's marker.
    if [ -e "$tmp" ] && ! rm -f "${tmp:?}" 2>/dev/null; then
        _SOT_LOCK_WHY="cannot remove an earlier $tmp"
        return 2
    fi
    if ! { printf '%s\n' "$_SOT_LOCK_ID" >|"$tmp"; } 2>/dev/null; then
        _SOT_LOCK_WHY="cannot write $tmp"
        return 2
    fi
    # A retransmitted LINK on NFSv3 answers "exists" for this call's own
    # link, so whether TARGET is now my temp file, both opened first so their
    # attributes are fresh, decides (review S4); `-ef` is a builtin, and BSD
    # stat has no `-c %h`.
    if err="$(LC_ALL=C link "$tmp" "$1" 2>&1)" \
        || { { : <"$tmp" && : <"$1"; } 2>/dev/null && [ "$tmp" -ef "$1" ]; }; then
        taken=1
    fi
    rm -f "${tmp:?}"
    [ -z "$taken" ] || return 0
    # Held is the link's own EEXIST, never "the target exists now": a holder
    # can release between the two, which would read as an error.
    case "$err" in *"File exists"*) return 1 ;; esac
    err="${err##*: }"
    _SOT_LOCK_WHY="cannot hard-link in ${1%/*}: ${err:-link failed}"
    return 2
}

# _sot_lock_fresh PATH — read PATH's record into _SOT_LOCK_READ, fresh from
# the server. Opening the folder first forces its GETATTR (close-to-open): a
# changed folder drops every cached lookup beneath it, so the record's own
# open looks it up on the wire; a plain stat or readlink was seen to stay
# stale on the shared home. The folder open is required only where a death
# can be proved. 0 = a record of six fields with a numeric pid; 1 = read
# whole, not a record; 2 = not read (the folder open required and failed, or
# the file's open or read failed). cat, not the builtin read, because only
# cat's exit tells a read error from the end of the file.
_sot_lock_fresh() {
    _SOT_LOCK_READ=""
    { : <"${1%/*}"; } 2>/dev/null || [ -z "${_SOT_LOCK_SELF:-}" ] || return 2
    { _SOT_LOCK_READ="$(cat -- "$1")"; } 2>/dev/null || { _SOT_LOCK_READ=""; return 2; }
    _SOT_LOCK_READ="${_SOT_LOCK_READ%%$'\n'*}"
    [[ "$_SOT_LOCK_READ" =~ ^[^:]*:[^:]*:[^:]*:[^:]*:[0-9]+:[^:]*$ ]]
}

# _sot_lock_judge ID — set _SOT_LOCK_VERDICT to DEAD, ALIVE or UNPROVABLE,
# and _SOT_LOCK_WHY to the reason a person reads.
_sot_lock_judge() {
    local name machine boot pidns pid start me_name="" me_machine="" me_boot="" me_pidns=""
    IFS=: read -r name machine boot pidns pid start <<<"$1" || true
    IFS=: read -r me_name me_machine me_boot me_pidns <<<"${_SOT_LOCK_SELF:-}" || true
    _SOT_LOCK_VERDICT=UNPROVABLE
    if [ -z "${_SOT_LOCK_SELF:-}" ]; then
        _SOT_LOCK_WHY="this box cannot prove a death"
    elif [ "$boot" != - ] && [ "$pidns" != - ] && [ "$boot" = "$me_boot" ] && [ "$pidns" = "$me_pidns" ]; then
        if _sot_lock_start "$pid"; then
            if [ "$start" != - ] && [ "$_SOT_LOCK_START" != "$start" ]; then
                _SOT_LOCK_VERDICT=DEAD; _SOT_LOCK_WHY="its pid now names another process"
            else
                _SOT_LOCK_VERDICT=ALIVE; _SOT_LOCK_WHY="it is running"
            fi
        elif [ ! -e "/proc/$pid" ]; then
            _SOT_LOCK_VERDICT=DEAD; _SOT_LOCK_WHY="it has exited"
        else
            _SOT_LOCK_WHY="its /proc entry cannot be read"
        fi
    elif [ "$machine" != - ] && [ "$boot" != - ] && [ "$me_boot" != - ] && [ "$name" != - ] \
        && [ "$machine" = "$me_machine" ] && [ "$name" = "$me_name" ] && [ "$boot" != "$me_boot" ]; then
        _SOT_LOCK_VERDICT=DEAD; _SOT_LOCK_WHY="its machine has rebooted since"
    elif [ "$machine" = - ] || [ "$machine" != "$me_machine" ] || [ "$name" != "$me_name" ]; then
        _SOT_LOCK_WHY="it is on another machine"
    else
        _SOT_LOCK_WHY="it is in another pid namespace"
    fi
}

# _sot_lock_vouch — 0 when the comm home's mount makes the fresh read fresh:
# ext2/3/4, xfs, btrfs, zfs, f2fs or tmpfs, or nfs/nfs4 without `nocto`; the
# mount on top, the last line of `findmnt -T`, when one is stacked. Reached
# only on Linux, except by comm-registry-lock-clear.sh, whose person vouches
# elsewhere.
_sot_lock_vouch() {
    local fs="" opts=""
    [ -n "${_SOT_LOCK_SELF:-}" ] || return 0
    read -r fs opts < <(_sot_findmnt -n -o FSTYPE,OPTIONS -T "${_SOT_REG_LOCK%/*}" 2>/dev/null | tail -n 1) || true
    case "$fs" in
        ext2|ext3|ext4|xfs|btrfs|zfs|f2fs|tmpfs|nfs|nfs4) ;;
        *) _SOT_LOCK_WHY="the comm home's filesystem (${fs:-unknown}) does not prove a fresh read"; return 1 ;;
    esac
    case ",$opts," in
        *,nocto,*) _SOT_LOCK_WHY="the comm home is mounted nocto, so no read of it is proved fresh"; return 1 ;;
    esac
}

# _sot_lock_step [--forced | DEADLINE] — one reclaim attempt against the lock
# as it stands. 0 = this step saw the lock go, retake at once; 1 = not (a lock
# released since the take, or one whose record could not be read, is a try,
# keeps the last holder named, clears _SOT_LOCK_BYHAND and sets _SOT_LOCK_GONE
# so the text says "was held"), with _SOT_LOCK_HOLDER (the ID the lock names,
# "" for none), _SOT_LOCK_WHO (the ID that blocks), _SOT_LOCK_BYHAND (1 when
# no reclaim can clear it, so only a person can, by hand: a directory, or a
# record read whole that does not parse) and _SOT_LOCK_WHY set for the FAILED
# line. The chain runs D, then the creator of reclaim.<D> if that one is dead
# too, and so on;
# every step past a marker needs its creator proved dead, so the live process
# holding the chain's last marker is the only one with authority over "the
# lock names a member of the chain". A marker naming me is one I took earlier
# in this wait. A marker naming a record the chain already holds, not mine,
# ends the walk: no reclaim can pass it (a daemon's own marker, left naming
# it when it died during its reclaim; review B1), so the lock is removed by
# hand. Past its first marker the walk stops at DEADLINE, a _sot_lock_now
# value. --forced (the clear command) takes a person's word for the
# unprovable holder the lock names, never against a proof that it is alive,
# and never for a marker's creator: a reclaimer that cannot be proved dead
# here may be pending on its own machine, and would remove the next holder's
# lock (review B1).
_sot_lock_step() {
    local x chain=() m c y r deadline=""
    [ "${1:-}" = --forced ] || deadline="${1:-}"
    _sot_lock_fresh "$_SOT_REG_LOCK"; r=$?
    if [ "$r" != 0 ]; then
        # First, as cat on a directory fails, so a directory arrives as 2.
        if [ -d "$_SOT_REG_LOCK" ]; then
            _SOT_LOCK_HOLDER=""; _SOT_LOCK_WHO=""; _SOT_LOCK_BYHAND=1; _SOT_LOCK_GONE=""
            _SOT_LOCK_WHY="held by an older version that records no holder"
            return 1
        fi
        # Not read: released, whether or not the lock exists now, as a live
        # writer can link it between the failed read and any existence test.
        if [ "$r" = 2 ]; then
            _SOT_LOCK_BYHAND=""; _SOT_LOCK_GONE=1
            [ -n "${_SOT_LOCK_HOLDER:-}" ] || _SOT_LOCK_WHY="its record could not be read, so it may have been released since"
            return 1
        fi
        _SOT_LOCK_HOLDER=""; _SOT_LOCK_WHO=""; _SOT_LOCK_BYHAND=1; _SOT_LOCK_GONE=""
        _SOT_LOCK_WHY="its record names no holder (${_SOT_LOCK_READ:-empty})"
        return 1
    fi
    _SOT_LOCK_HOLDER="$_SOT_LOCK_READ"; _SOT_LOCK_WHO=""; _SOT_LOCK_BYHAND=""; _SOT_LOCK_GONE=""; x="$_SOT_LOCK_READ"
    while :; do
        chain+=("$x")
        [ "${#chain[@]}" -gt 1 ] && _sot_lock_is_me "$x" && break
        if [ "${#chain[@]}" -gt 1 ] && [ -n "$deadline" ] \
            && { ! _sot_lock_now 0 2>/dev/null || [ "$_SOT_LOCK_NOW" -ge "$deadline" ]; }; then
            _SOT_LOCK_WHO="$x"; _SOT_LOCK_WHY="its reclaim chain was still being walked at the deadline"
            return 1
        fi
        _sot_lock_judge "$x"
        [ "${1:-}" = --forced ] && [ "${#chain[@]}" = 1 ] && [ "$_SOT_LOCK_VERDICT" = UNPROVABLE ] && _SOT_LOCK_VERDICT=DEAD
        if [ "$_SOT_LOCK_VERDICT" != DEAD ]; then
            _SOT_LOCK_WHO="$x"
            [ "$x" = "$_SOT_LOCK_HOLDER" ] \
                || _SOT_LOCK_WHY="it is dead, but its reclaim by ${x%%:*} pid $(_sot_lock_field "$x" 5) did not finish: $_SOT_LOCK_WHY"
            return 1
        fi
        if [ "${#chain[@]}" = 1 ] && ! _sot_lock_vouch; then _SOT_LOCK_WHO="$x"; return 1; fi
        m="$_SOT_REG_LOCK.reclaim.${x//:/.}"
        if _sot_lock_take "$m"; then break; else c=$?; fi
        if [ "$c" = 2 ] || ! _sot_lock_fresh "$m"; then
            _SOT_LOCK_WHO="$x"; _SOT_LOCK_WHY="its reclaim marker $m cannot be read${_SOT_LOCK_WHY:+ ($_SOT_LOCK_WHY)}"
            return 1
        fi
        x="$_SOT_LOCK_READ"
        for y in "${chain[@]}"; do
            if [ "$y" = "$x" ] && ! _sot_lock_is_me "$x"; then
                _SOT_LOCK_HOLDER="" _SOT_LOCK_WHO="" _SOT_LOCK_BYHAND=1 _SOT_LOCK_GONE=""
                _SOT_LOCK_WHY="its reclaim marker $m names $x, which its reclaim chain already holds, so no reclaim can pass it"
                return 1
            fi
        done
    done
    sleep "${SOT_COMM_TEST_LOCK_SETTLE:-1}"
    if ! _sot_lock_fresh "$_SOT_REG_LOCK"; then
        [ -e "$_SOT_REG_LOCK" ] || return 0
        _SOT_LOCK_WHY="its record changed during the reclaim"
        return 1
    fi
    for x in "${chain[@]}"; do
        if [ "$_SOT_LOCK_READ" = "$x" ]; then
            rm -f "${_SOT_REG_LOCK:?}"
            return 0
        fi
    done
    _SOT_LOCK_HOLDER="$_SOT_LOCK_READ"; _SOT_LOCK_WHY="it was taken again during the reclaim"
    return 1
}

_sot_lock_field() {  # ID N — the ID's Nth colon field
    local f=()
    IFS=: read -r -a f <<<"$1" || true
    printf '%s' "${f[$2 - 1]:--}"
}

# _sot_lock_fail_text — the one FAILED line for the lock as _sot_lock_step
# last saw it: the holder's host, pid and start tick, and the recovery.
_sot_lock_fail_text() {
    local age="unknown" mtime="" who="${_SOT_LOCK_WHO:-${_SOT_LOCK_HOLDER:-}}"
    mtime="$(stat -c %Y "$_SOT_REG_LOCK" 2>/dev/null)" || mtime=""
    [ -n "$mtime" ] && age="$(( $(date +%s) - mtime ))s"
    if [ -n "${_SOT_LOCK_BYHAND:-}" ]; then
        echo "ERROR: registry lock $_SOT_REG_LOCK still held ($age old): $_SOT_LOCK_WHY. If its holder is dead, remove $_SOT_REG_LOCK by hand and retry."
        return 0
    fi
    if [ -z "${_SOT_LOCK_HOLDER:-}" ]; then
        echo "ERROR: registry lock $_SOT_REG_LOCK was not taken by the deadline: $_SOT_LOCK_WHY. Retry."
        return 0
    fi
    if [ -n "${_SOT_LOCK_GONE:-}" ]; then
        echo "ERROR: registry lock $_SOT_REG_LOCK was held by ${_SOT_LOCK_HOLDER%%:*} pid $(_sot_lock_field "$_SOT_LOCK_HOLDER" 5) start $(_sot_lock_field "$_SOT_LOCK_HOLDER" 6) when last read ($_SOT_LOCK_WHY); it may have been released since. Retry."
        return 0
    fi
    echo "ERROR: registry lock $_SOT_REG_LOCK is held by ${_SOT_LOCK_HOLDER%%:*} pid $(_sot_lock_field "$_SOT_LOCK_HOLDER" 5) start $(_sot_lock_field "$_SOT_LOCK_HOLDER" 6) ($age old): $_SOT_LOCK_WHY. If it is dead, run any comm command on ${who%%:*}, or run comm-registry-lock-clear.sh."
}
