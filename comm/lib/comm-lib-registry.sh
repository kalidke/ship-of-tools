# comm-lib-registry.sh: the registry file: ensure_home, the writers, the reads and a row's status.
# Sourced by comm-lib.sh; defines functions only.

# _sot_comm_tighten — close to group and other what an older release left open in the comm folder (ADR 0049, User
# isolation). It touches only the layout's own entries (_sot_comm_own), never an unknown file or folder, and refuses,
# changing nothing, a comm folder that is the root, the home folder or a git checkout (a mistaken SOT_COMM_HOME). Not
# on Windows, where the profile's access list is the mechanism. It walks only while the folder itself is open: once it
# is 0700 nothing new is reached through it. The warning comes from a re-check of the end state, so a file that
# vanished under the chmod is no failure; the caller goes on either way.
_sot_comm_tighten() {
    _sot_is_windows && return 0
    local why left
    why="$(_sot_comm_refusal)"
    if [ -n "$why" ]; then
        echo "WARNING: the comm folder $COMM_HOME is $why, so its permissions were not changed" >&2
        return 0
    fi
    [ -n "$(_sot_comm_own root)" ] || return 0
    _sot_comm_own fix >/dev/null 2>&1 || true
    left="$(_sot_comm_own print | head -n 1)" || left=""
    [ -z "$left" ] || echo "WARNING: the comm folder $COMM_HOME could not be made private: $left is still $(ls -ld -- "$COMM_HOME/$left" 2>/dev/null | cut -c1-10)" >&2
    return 0
}

# _sot_comm_refusal — why the comm folder must not be tightened (it names a folder that is not a comm folder), or nothing.
_sot_comm_refusal() {
    local phys home=""
    phys="$(cd -- "$COMM_HOME" 2>/dev/null && pwd -P)" || return 0
    [ -z "${HOME:-}" ] || home="$(cd -- "$HOME" 2>/dev/null && pwd -P)"
    if [ "$phys" = / ]; then echo "the root folder"
    elif [ -n "$home" ] && [ "$phys" = "$home" ]; then echo "the home folder"
    elif [ -e "$phys/.git" ]; then echo "a git checkout"
    fi
}

# _sot_comm_own root|print|fix — the comm layout's own entries open to group or other, and nothing else: the folder,
# inbox/ read/ self/ state/ probe/ and the folders in probe/ (a probe row's project root); registry.json,
# registry.json.tmp and registry.json.new.*, .registry.lock and its .tmp.* and .reclaim.* files, inbox-lock-manager and
# .inbox-lock-manager.*, gh-device-auth.json; and the files of inbox/ (.jsonl, .lock), read/ (.cursor), self/ (.txt) and
# state/ (all). This is comm/PROTOCOL.md's layout block, and test-comm-private.sh's table lays one of each. find is handed the layout folders and
# descends one level itself, so a layout folder that is a symlink is never followed; bin/ and VERSION are never in the
# list. `root` prints the folder itself if it is open, `print` every entry open, `fix` removes group and other bits,
# the folder itself last: a pass cut short leaves it open, and the next call walks again.
_sot_comm_own() {
    ( cd -- "$COMM_HOME" 2>/dev/null || exit 0
      local open=( \( -perm -040 -o -perm -020 -o -perm -010 -o -perm -004 -o -perm -002 -o -perm -001 \) )
      local act=( -print ) one=( -maxdepth 1 -mindepth 1 -type f )
      [ "$1" != fix ] || act=( -exec chmod go-rwx {} + )
      if [ "$1" = root ]; then find . -prune "${open[@]}" -print; exit 0; fi
      find inbox read self state probe -maxdepth 0 -type d "${open[@]}" "${act[@]}"
      find probe -maxdepth 1 -mindepth 1 -type d "${open[@]}" "${act[@]}"
      find . "${one[@]}" \( -name inbox-lock-manager -o -name '.inbox-lock-manager.*' -o -name gh-device-auth.json \
          -o -name registry.json -o -name registry.json.tmp -o -name 'registry.json.new.*' \
          -o -name .registry.lock -o -name '.registry.lock.tmp.*' -o -name '.registry.lock.reclaim.*' \) "${open[@]}" "${act[@]}"
      find inbox "${one[@]}" \( -name '*.jsonl' -o -name '*.lock' \) "${open[@]}" "${act[@]}"
      find read "${one[@]}" -name '*.cursor' "${open[@]}" "${act[@]}"
      find self "${one[@]}" -name '*.txt' "${open[@]}" "${act[@]}"
      find state "${one[@]}" "${open[@]}" "${act[@]}"
      find . -maxdepth 0 -type d "${open[@]}" "${act[@]}"
      exit 0 ) 2>/dev/null
}

ensure_home() {
    mkdir -p "$COMM_HOME" "$INBOX_DIR" "$SELF_DIR" "$READ_DIR"
    _sot_comm_tighten
    # Create only, never truncate: `test -f` is false on any stat error (an
    # ESTALE during another host's rename), and a plain `>` then wiped a live
    # registry. So the skeleton is written to its own tmp (noclobber: O_EXCL, so
    # two writers never share one), fsynced, and published by link(2), which
    # fails on any existing name and never replaces a file: a wrong stat fails
    # at the link. perl's link, not ln, which links INTO a directory at the path.
    # An empty registry is never repaired here.
    [ -f "$REGISTRY" ] && return 0
    local tmp="$REGISTRY.new.$$.$RANDOM"
    ( set -C; printf '{"protocol_version": %s, "agents": {}}\n' "$PROTOCOL_VERSION" > "$tmp" ) 2>/dev/null \
        && _sot_fsync "$tmp" >/dev/null 2>&1 \
        && perl -e 'link($ARGV[0], $ARGV[1]) or exit 1' "$tmp" "$REGISTRY" 2>/dev/null
    rm -f "${tmp:?}"
    return 0
}

# --- registry mutators (call inside with_lock) ---
registry_put() {  # name objJSON
    # F7 (Codex review): never write an empty/blank handle — a derivation
    # bug or a corrupt-registry jq failure upstream is an ERROR, not a
    # claim of "". Last line of defense regardless of how a caller got here.
    if [ -z "$1" ]; then
        echo "registry_put: refusing to write an empty/blank handle" >&2
        return 1
    fi
    registry_replace '.agents[$n] = $o' --arg n "$1" --argjson o "$2"
}
registry_del() {  # name
    registry_replace 'del(.agents[$n])' --arg n "$1"
}
registry_touch() {  # name — bump last_seen if present
    local ts; ts="$(now_iso)"
    registry_replace 'if .agents[$n] then .agents[$n].last_seen = $t else . end' --arg n "$1" --arg t "$ts"
}

# registry_replace FILTER [JQ_OPTIONS...] — THE one registry write; call inside with_lock.
# The tmp is renamed only if it is ONE document with an .agents object and has been fsynced.
# jq emits nothing for an empty file and fails on an unparseable one, so an unreadable
# read can never produce a tmp that passes: it aborts, and the registry's inode and bytes stay as they were.
# The check slurps: jq 1.6's -e exits 0 on a file with no document, and judges two by the last.
registry_replace() {
    local filter="$1" why; shift
    if sot_registry_bytes | jq "$@" "$filter" > "$REGISTRY.tmp" 2>/dev/null \
       && jq -e -s 'length == 1 and (.[0].agents | type == "object")' "$REGISTRY.tmp" >/dev/null 2>&1; then
        if ! why="$(_sot_fsync "$REGISTRY.tmp" 2>&1)"; then why="could not be flushed ($why)"
        elif why="$(mv "$REGISTRY.tmp" "$REGISTRY" 2>&1)"; then return 0
        else why="could not be renamed into place ($why)"; fi
        rm -f "${REGISTRY:?}.tmp"; echo "FAILED: the registry update $why, so nothing was written" >&2; return 1
    fi
    rm -f "${REGISTRY:?}.tmp"; echo "FAILED: the registry could not be read or updated, so nothing was written" >&2; return 1
}
# _sot_fsync FILE — FILE's data on the server before a rename publishes it (the sync _sot_append_whole does).
# ($f is declared in its own statement: a `my` is not in scope until the next one.)
_sot_fsync() { perl -MIO::Handle -e 'my $f; open($f, "+<", $ARGV[0]) && $f->sync && close($f) or do { print STDERR "$ARGV[0]: $!\n"; exit 1 }' "$1"; }

# sot_registry_bytes [FILE] — the registry's bytes (FILE, default $REGISTRY) on stdout: THE read under
# every registry read and write, this library's and the standalone hooks' (they source it in a subshell).
# 0 = the bytes, whole; 1 = absent; 2 = unreadable. Nothing is on stdout but for 0.
# An NFSv4 client can get ESTALE (stale file handle) from a read after its open succeeded, when another
# host renames a new registry over the file: the two-host test measured 16 in about 2,000 reads on the
# first host before this retry, 0 in about 9,000 on the peer. So a try that FAILED (an open error other
# than no such file, a read error even after some bytes, which are discarded, or zero bytes) opens the
# folder, which revalidates it, and opens the file by path again, never another read of the old
# descriptor: up to 3 retries in about 200 ms. No good read by then is unreadable, never absent.
# Absent is told at the open, never by a stat: the first try's open said "No such file or directory"
# (LC_ALL=C, so the C library's text). A later try's is a file that vanished mid-retry: a failed try.
# Zero bytes stays in the rule because an empty file is never a registry (every writer fsyncs a checked
# tmp before its rename, and ensure_home its skeleton before its link), but no zero-byte read has ever
# been observed. Non-empty bytes read whole are
# never retried, parseable or not. The open is a redirection and the read is cat, whose exit says a
# read failed; bash 3.2 and git-bash have both.
# SOT_COMM_TEST_RETRY_LOG (tests only): a file that gets "retry N" per retry and "resolved" per read a
# retry made good. Unset, nothing is written.
sot_registry_bytes() {
    local f="${1:-$REGISTRY}" out try=0 nl=$'\n'
    while :; do
        { out="$(LC_ALL=C; { cat 2>/dev/null && printf '\n+' || printf '\n-'; } 2>&1 < "$f")" || :; } 2>/dev/null
        case "$out" in
            *"$nl+") out="${out%??}"
                     [ -n "$out" ] && { [ "$try" -eq 0 ] || _sot_retry_note resolved; printf '%s' "$out"; return 0; } ;;
            *": No such file or directory") [ "$try" -eq 0 ] && return 1 ;;
        esac
        [ "$try" -lt 3 ] || return 2
        try=$((try + 1)); _sot_retry_note "retry $try"
        { [ "$try" -eq 1 ] || sleep 0.1; : < "${f%/*}"; } 2>/dev/null || :
    done
}
_sot_retry_note() { [ -z "${SOT_COMM_TEST_RETRY_LOG:-}" ] || echo "$1" >> "$SOT_COMM_TEST_RETRY_LOG" 2>/dev/null || :; }

# sot_heartbeat_fresh STAMP — rc 0 exactly when the daemon's `heartbeat_fresh`
# would say so (pinned by `heartbeat_agrees_with_the_shell`): STAMP is
# `YYYY-MM-DDTHH:MM:SSZ` in ASCII digits, and sorts after the same shape of
# `now - COMM_LIVE_SECS`, compared as bytes. Both writers stamp that one
# fixed-width shape, so string order is time order. A stamp that is absent or
# not that shape is no heartbeat. If `date` cannot print the cutoff the answer
# is "not fresh": a broken `date` can never give a false `filed`.
sot_heartbeat_fresh() {
    local stamp="${1:-}" cutoff n
    local LC_ALL=C
    case "$stamp" in
        [0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9]Z) ;;
        *) return 1 ;;
    esac
    n=$(( $(date -u +%s) - COMM_LIVE_SECS ))
    cutoff="$(date -u -d "@$n" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null)" \
        || cutoff="$(date -u -r "$n" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null)" || return 1
    [ "${#cutoff}" -eq 20 ] || return 1
    [[ "$stamp" > "$cutoff" ]]
}

# sot_handle_live HANDLE — rc 0 when the registry's .agents[HANDLE].last_seen is
# fresh (sot_heartbeat_fresh), else rc 1. An absent row, an absent or
# unparseable last_seen all mean not live: it never fabricates liveness. Asks
# the registry only (no daemon round trip), so comm-join.sh's stranding warning
# works on a box with no daemon reachable.
sot_handle_live() {
    local last_seen
    last_seen="$(sot_registry_read "$1" | sot_jq -r '.last_seen | strings | select(length == 20)' 2>/dev/null)" || return 1
    sot_heartbeat_fresh "$last_seen"
}

# registry_del_if_provisional NAME WANT_ROOT WANT_NONCE — conditionally
# delete NAME's row, but ONLY if it's STILL provably the exact provisional
# row identified by WANT_ROOT + WANT_NONCE (status "spawning" is implied —
# a provisional row is always spawning; a real join or an explicit
# claimant always overwrites both root and status/removes the nonce as it
# writes a normal row). Call under with_lock. Exists so a spawn's rollback
# can never delete a NEWER row that has since replaced the provisional one
# (Codex review PR #148 round 2, finding 1 — reproduced by the reviewer:
# an unconditional `registry_del "$NAME"` deleted a live `status:"idle"`
# row the child had already written for real, turning a successful join
# into `null`). Returns:
#   0 — deleted (it was still ours)
#   1 — deletion itself failed (registry_del's jq/mv step)
#   2 — NOT deleted: the row no longer matches what was claimed (or
#       WANT_NONCE/NAME is empty) — left untouched; this is the common,
#       expected outcome once a real join has happened, not an error
registry_del_if_provisional() {
    local name="$1" want_root="$2" want_nonce="$3"
    local row rc=0 cur_status cur_root cur_nonce
    [ -n "$name" ] && [ -n "$want_nonce" ] || return 2
    # Unreadable is 1 (the caller's "check by hand"), never the absent 2.
    row="$(sot_registry_read "$name")" || rc=$?
    case "$rc" in 0) ;; 1) return 2 ;; *) return 1 ;; esac
    cur_status="$(printf '%s' "$row" | sot_jq -r '.status // ""' 2>/dev/null)"
    cur_root="$(printf '%s' "$row" | sot_jq -r '.root // ""' 2>/dev/null)"
    cur_nonce="$(printf '%s' "$row" | sot_jq -r '.nonce // ""' 2>/dev/null)"
    if [ "$cur_status" != "spawning" ] || [ "$cur_root" != "$want_root" ] || [ "$cur_nonce" != "$want_nonce" ]; then
        return 2
    fi
    registry_del "$name"
}

# sot_registry_entry_status NAME — tagged status of the registry row for
# NAME (Codex review simplicity audit: replaces a magic sentinel string
# with tagged output, so "no row" and "row present but root unknown" can
# never be confused with each other or with an actual, if empty, root
# value):
#   "absent\t"          — NAME has no row at all
#   "present\t<root>"   — NAME has a row; <root> is "" for a legacy row
#                         that predates this feature (unknown root)
#   "error\t"           — the registry could not be read/parsed (Codex
#                         review round-3 finding 1): jq failing (malformed
#                         JSON, unreadable file, an NFS hiccup) used to
#                         print NOTHING, which read back as an empty
#                         string indistinguishable from "absent" to every
#                         caller — letting a pane-keyed legacy self-file
#                         self-heal on a basename match with the registry
#                         effectively unconsultable. Callers MUST treat
#                         "error" as NO EVIDENCE, never as "absent".
sot_registry_entry_status() {
    local row rc=0 root
    row="$(sot_registry_read "$1")" || rc=$?
    case "$rc" in
        0) root="$(printf '%s' "$row" | sot_jq -r '.root // ""' 2>/dev/null)" || { printf 'error\t\n'; return 0; }
           printf 'present\t%s\n' "$root" ;;
        1) printf 'absent\t\n' ;;
        *) printf 'error\t\n' ;;
    esac
}

# sot_registry_read [HANDLE] — THE unlocked registry read. No HANDLE: the registry, compact.
# HANDLE: that row, compact. 0 present; 1 absent (it parsed, no such row); 2 unreadable
# (missing, empty, not JSON, not one document, or no .agents object), with nothing on stdout.
# 2 never means absent. It slurps and reads jq's output, never its exit code alone: jq 1.6
# exits 0 on a file with no document.
sot_registry_read() {
    local out
    out="$(sot_registry_bytes | sot_jq -s -r --arg n "${1-}" --arg one "${1+1}" '
        if length != 1 or (.[0].agents | type) != "object" then "unreadable"
        else .[0] | if $one == "" then "present\t" + tojson
        elif .agents | has($n) then "present\t" + (.agents[$n] | tojson) else "absent" end end' \
        2>/dev/null)" || return 2
    case "$out" in present$'\t'*) printf '%s\n' "${out#present$'\t'}" ;; absent) return 1 ;; *) return 2 ;; esac
}
