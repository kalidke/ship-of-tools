# comm-lib-base.sh: the library's base: platform test, the comm folder's paths, clock, tool checks, jq and host helpers, ages.
# Sourced by comm-lib.sh; defines functions and globals only.

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

PROTOCOL_VERSION=1

# The one comm-folder rule; the daemon spells it as `sot_comm_home` (rust/backend/src/paths.rs).
COMM_HOME="${SOT_COMM_HOME:-$HOME/.sot-comm}"
REGISTRY="$COMM_HOME/registry.json"
INBOX_DIR="$COMM_HOME/inbox"
SELF_DIR="$COMM_HOME/self"
READ_DIR="$COMM_HOME/read"
# The registry lock (see with_lock): a FILE naming its holder.
_SOT_REG_LOCK="$COMM_HOME/.registry.lock"

now_iso() { date -u +%Y-%m-%dT%H:%M:%SZ; }

# sot_mail_tools — the tools every path that reads mail runs, one line: jq
# parses every frame; flock and perl are the inbox's read and write lock,
# Linux only; before bash 5 (macOS's 3.2), perl's Time::HiRes is the registry
# lock's clock. Poll, session start and the Stop hook all take their list here.
sot_mail_tools() {
    local t=jq
    [ "$(uname -s 2>/dev/null)" != Linux ] || t="jq flock perl"
    [ "${BASH_VERSINFO[0]}" -ge 5 ] || t="$t Time::HiRes"
    echo "$t"
}

# sot_require_tools PATH_NAME TOOL... — say, on stderr, one line per TOOL that
# is not on PATH (a TOOL with `::` is a perl module perl cannot load), and
# return nonzero if any is missing. A session whose jq,
# flock or perl is missing never sees its mail and used to be told nothing:
# each path that reads mail (comm-poll, session start, the Stop hook) checks
# the tools IT runs before first use and shows this line to the session.
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

# sot_jq ARGS... — run jq, but normalise ITS OWN OUTPUT so a caller that
# captures a single field (command substitution) or splits several
# records (mapfile / `while read`) never keeps a stray carriage return
# glued to the value. On Windows, a native (non-MSYS) `jq.exe` opens
# stdout in text mode and rewrites every \n it writes to \r\n; bash's own
# consumption idioms keep that \r — command substitution strips only the
# final \n, and mapfile/`read` split on \n only — so a handle, host,
# workspace id or registry root read back this way never compares equal
# to the clean string it should match (field report, 2026-09-27: a
# handle read back as "admiral-kitt\r" made every send from that box
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

# sot_fmt_age SECONDS -> "12s"/"6m"/"32m"/"8h"/"3d" (no "ago" suffix -- every
# caller supplies its own wording, since the same figure reads differently in
# a success line vs a timeout message). A negative age (clock skew) floors to
# 0 rather than printing garbage.
sot_fmt_age() {
    local s="$1"
    [ "$s" -lt 0 ] 2>/dev/null && s=0
    if   [ "$s" -lt 60 ];    then echo "${s}s"
    elif [ "$s" -lt 3600 ];  then echo "$((s / 60))m"
    elif [ "$s" -lt 86400 ]; then echo "$((s / 3600))h"
    else                          echo "$((s / 86400))d"
    fi
}

# --- MSYS2 argv-conversion guard for jq values that can legitimately
# start with "/" ---
#
# capsule-comm-identity fix, field-measured on a Windows git-bash box: when
# a NATIVE (non-MSYS) jq.exe is invoked, MSYS2's argv-to-Windows-path
# conversion rewrites any ARGV ELEMENT that starts with "/" into a Windows
# path before jq ever sees it — verified through the real relay, a message
# beginning "/sot-session-start ..." arrived in the peer's inbox mangled
# into a filesystem path. The rule is exact and easy to miss in ad hoc
# testing: only the FIRST character of the whole argument matters (a bare
# "/" corrupts too); "./", "~/", "//server", and every MID-string slash
# are untouched; on a MULTI-LINE value only the FIRST line is mangled —
# every later line survives intact, which is why this read as an isolated
# typo rather than a systematic corruption. `--arg NAME "$value"` passes
# $value as its own argv element, so any of the THREE values in this
# codebase that can genuinely start with "/" — a message body
# (comm-send.sh, comm-relay.sh) and a project root (comm-join.sh) — must
# never go through `--arg`. Every OTHER `--arg` (handles, hosts, ids,
# timestamps) is drawn from a restricted charset that can never start
# with "/" and is untouched by this fix.
#
# sot_jq_rawfile VALUE — writes VALUE verbatim (no added newline) to a
# fresh temp file and prints its path, for use as `jq --rawfile NAME
# <path> ...` in place of `--arg NAME "$VALUE"`: the risky VALUE now lives
# in the file's CONTENT, read by jq via fread — never an argv element, so
# never subject to the conversion. The file PATH argument itself is left
# as an ordinary argument and SHOULD still convert when it starts with
# "/" (that's the well-behaved half of the same MSYS2 mechanism — it's
# what lets a native jq.exe find the file at all), so no
# MSYS2_ARG_CONV_EXCL or similar exclusion is involved. The caller owns
# the returned path and MUST `rm -f` it once jq has run.
sot_jq_rawfile() {
    local f
    f="$(mktemp "${TMPDIR:-/tmp}/sot-comm-jq.XXXXXX" 2>/dev/null)" || {
        echo "sot_jq_rawfile: could not create a temp file for a jq --rawfile value" >&2
        return 1
    }
    if ! printf '%s' "$1" > "$f" 2>/dev/null; then
        echo "sot_jq_rawfile: write to temp file '$f' failed" >&2
        rm -f "${f:?}" 2>/dev/null
        return 1
    fi
    printf '%s' "$f"
}

# sot_host — this shell's DECLARED host name for the wire only (ADR 0046
# decision 1, manager review S1/S2): the ONE resolver matching
# sot_log::state_dir::host_name() on the Rust side exactly — `$SOT_SELF_HOST`
# verbatim if set and non-empty (a NEW variable: `SOT_HOST` already means
# the SSH target a remote frontend dials, `scripts/launch-sot.ps1`/
# `launch-sot.sh` — reusing it here would silently rename a frontend's
# declared identity to whatever it dials), else the first `.`-label of
# `hostname -s`, lowercased. Feeds ONLY `sot_hello_frame`'s wire `host`
# field and display/logs — never an address or on-disk namespace: no
# on-disk namespace changes this sprint (S1), so comm-context.sh's own
# `HOST` (the self-file key, handle derivation) does NOT call this;
# `hostname -s` there stays completely independent, exactly as on main.
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
    # after taking the first label) — manager review round 2: the two
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
