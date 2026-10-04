# comm-lib-identity.sh: identity: the self file, the routable-identity gate, slugs and derived handles.
# Sourced by comm-lib.sh; defines functions only.

# --- self-file writer (shared by comm-join.sh and comm-context.sh's
# read-side self-heal) ---

# sot_write_self_file SELF_FILE NAME REPO ROOT — write the v2 self-file
# format (identity line, then `repo=`, then `root=`) to SELF_FILE via a
# same-directory temp file + checked `mv`, never an in-place `>`
# truncation (Codex review round-1 finding 3: the old in-place write left a
# read-only self-file silently unwritten — the redirection failed, nothing
# checked its exit status, and the caller went on to print a success
# message anyway — and could leave a torn/zero-byte file if interrupted
# mid-write). The temp file lives NEXT TO SELF_FILE so the final `mv` is a
# same-filesystem rename: atomic, no partial-write window a concurrent
# reader could observe.
#
# Both write sites — this self-heal path in comm-context.sh and the
# ordinary join-write in comm-join.sh — route through this ONE function so
# there is a single place that gets the atomicity right, rather than two
# copies that could drift.
#
# No cross-process lock: the self-file's "nopane" slot is deliberately
# SHARED across every no-workspace shell on a host (see
# comm-context.sh's nopane note) and is last-writer-wins BY DESIGN — every
# read of it is independently re-validated (against root=, or against
# repo=/the registry for a legacy file), so a slot two shells raced to
# write is caught on its next read rather than silently trusted either
# way. Serializing the write here would only slow down an already-safe
# race, not close a real hazard.
#
# Returns 0 only once SELF_FILE has been VERIFIABLY replaced with the new
# content; nonzero (with a reason on stderr) otherwise, and the original
# SELF_FILE is left untouched (the failed temp file is cleaned up, never
# left as e.g. a stray `.tmp.*` sibling). Callers MUST treat a nonzero
# return as "did not persist" — never report success on it. A refusal by the
# agreement guard below is return 3 specifically, so a caller can tell "this
# slot belongs to another project" from "the disk said no".
#
# sot_self_file_project_conflict SELF_FILE REPO ROOT — 0 (and one line on
# stderr naming the incumbent) when SELF_FILE already holds an identity
# claimed for a DIFFERENT project than (REPO, ROOT), 1 otherwise. This is the
# WRITE side of the read-side staleness matrix in comm-context.sh: the slot
# path is keyed by the workspace row in the ENVIRONMENT while the identity
# written into it is derived from the shell's CWD, the two derivations are
# never compared, and so any process whose $SOT_WORKSPACE_ID names one row
# while its cwd sits in another row's repo used to write that repo's handle
# into the other row's slot — after which the other session reads a handle
# that is not its own and directed mail is filed for the wrong reader (the
# inbox is keyed by handle, so the read-side matrix catches it only AFTER the
# misdelivery). `root=` decides when the slot has one; `repo=` decides for a
# legacy slot; a slot with neither (the ancient one-line format) is no
# evidence and never a conflict. The SHARED `$HOST__nopane.txt` slot is
# exempt: it is last-writer-wins BY DESIGN (see the note above), every
# no-pane shell on the host writes it whatever project it sits in, and a
# guard there would refuse ordinary use.
sot_self_file_project_conflict() {
    local self_file="$1" repo="$2" root="$3"
    case "$self_file" in *__nopane.txt) return 1 ;; esac
    [ -f "$self_file" ] || return 1
    local -a lines; mapfile -t lines < "$self_file" 2>/dev/null || return 1
    local name="${lines[0]:-}" repo_line="${lines[1]:-}" root_line="${lines[2]:-}" claim=""
    [ -n "$name" ] || return 1
    case "$root_line" in root=?*) claim="root='${root_line#root=}'"
        [ "${root_line#root=}" = "$root" ] && return 1 ;;
    esac
    if [ -z "$claim" ]; then
        case "$repo_line" in repo=?*) claim="repo='${repo_line#repo=}' (legacy slot, no root=)"
            [ "${repo_line#repo=}" = "$repo" ] && return 1 ;;
        esac
    fi
    [ -n "$claim" ] || return 1
    echo "the identity slot '$self_file' already names @$name, claimed for $claim" >&2
    return 0
}
sot_write_self_file() {
    local self_file="$1" name="$2" repo="$3" root="$4" repin="${5:-0}" tmp
    if [ "$repin" != 1 ] && sot_self_file_project_conflict "$self_file" "$repo" "$root"; then
        echo "sot_write_self_file: REFUSING to write '$name' (repo='$repo', root='$root') over it — a slot keyed by one row must not come to name another project's session, or that row reads mail addressed to this one. If this row really is '$repo' now, re-run the join with --repin." >&2
        return 3
    fi
    tmp="$(mktemp "${self_file}.tmp.XXXXXX" 2>/dev/null)" || {
        echo "sot_write_self_file: could not create a temp file next to '$self_file' (directory missing or not writable?)" >&2
        return 1
    }
    if ! printf '%s\nrepo=%s\nroot=%s\n' "$name" "$repo" "$root" > "$tmp" 2>/dev/null; then
        echo "sot_write_self_file: write to temp file '$tmp' failed (disk full? permissions?)" >&2
        rm -f "${tmp:?}" 2>/dev/null
        return 1
    fi
    if ! mv -f "$tmp" "$self_file" 2>/dev/null; then
        echo "sot_write_self_file: could not move '$tmp' into place at '$self_file'" >&2
        rm -f "${tmp:?}" 2>/dev/null
        return 1
    fi
    return 0
}

# sot_raw_host VAR — set VAR to the host part of a self file's name: raw
# `hostname -s`, case kept, $SOT_COMM_TEST_HOST first when set. Not sot_host.
sot_raw_host() {
    if [ -n "${SOT_COMM_TEST_HOST:-}" ]; then
        printf -v "$1" '%s' "$SOT_COMM_TEST_HOST"
        return 0
    fi
    local _sot_rh _sot_rc
    _sot_rh="$(hostname -s 2>/dev/null || hostname)"
    _sot_rc=$?
    printf -v "$1" '%s' "$_sot_rh"
    return "$_sot_rc"
}

# sot_capsule_workspace_id — print the row id THIS SHELL'S IDENTITY names,
# or print nothing and return 1 when it names none. The identity is the
# pinned $SOT_COMM_SELF_FILE, which comm-context.sh names
# "<host>__<workspace_id>.txt", so the row id travels with the identity
# instead of with the environment. $SOT_WORKSPACE_ID answers only where no
# self file is pinned — there the slot comm-context.sh derives is keyed by
# that same ambient id, so the two agree by construction. "nopane" is the
# literal placeholder comm-context.sh writes for a non-capsule shell, never
# a real id — treated the same as absent. A host label or an id may itself hold
# "__", so when the name has more than one, this host's own label (sot_host) is
# stripped first; with no match the cut is at the first "__".
#
# The order is load-bearing and ran the other way until 2026-09-28. A test
# or a lane pins its own scratch identity but inherits $SOT_WORKSPACE_ID
# from the session that launched it, so the ambient id let every hermetic
# suite declare its throwaway handle into the live row it happened to run
# inside — ninety-two such declarations in one morning, and the last one
# left that row naming a handle no status lookup could resolve, which the
# owner saw as a permanently grey badge on a session that was working fine.
# An identity that does not name a row has no row to declare into.
sot_capsule_workspace_id() {
    local base="${SOT_COMM_SELF_FILE:-}"
    if [ -z "$base" ]; then
        [ -n "${SOT_WORKSPACE_ID:-}" ] || return 1
        printf '%s\n' "$SOT_WORKSPACE_ID"
        return 0
    fi
    base="$(basename "$base")"
    case "$base" in
        *__*.txt) ;;
        *) return 1 ;;
    esac
    local id="${base#*__}" host=""
    case "$id" in
        *__*) host="$(sot_host 2>/dev/null)" || host=""
              if [ -n "$host" ]; then
                  case "$base" in "$host"__*) id="${base#"$host"__}" ;; esac
              fi ;;
    esac
    id="${id%.txt}"
    [ -n "$id" ] && [ "$id" != "nopane" ] || return 1
    printf '%s\n' "$id"
}

# --- sender identity: NAME resolved is not enough, it must be ROUTABLE ---
#
# Codex review round-2 finding 4/C: comm-send.sh, comm-relay.sh, and
# comm-bootstrap.sh each carried their OWN "resolved identity" check, and
# each treated a merely NONEMPTY $NAME as good enough. That's insufficient:
# a self-file can resolve NAME locally (it passed comm-context.sh's own
# root=/repo= validation) while the registry itself has no matching row
# for it at all (evicted, or a failed registry write) or — worse — a row
# whose root belongs to a DIFFERENT project (this handle was reclaimed
# elsewhere). Either way, sending under it stamps a from-handle a reply
# can't route back to, or routes it to the wrong session. "Resolved" now
# means ROUTABLE: NAME nonempty AND a registry row for it exists AND (that
# row's root is empty — a legacy row, allowed during the migration window
# — OR it matches this project's canonical root).
#
# ONE helper, called by all three scripts, replacing three separately
# drifting diagnostic essays with the invariant plus the exact recovery
# command. Must be called BEFORE any endpoint/socket/transport resolution
# (Codex review round-2 SHOULD-FIX 2) so an unresolved sender always sees
# THIS refusal, never an unrelated daemon/socket error.
#
# Prints nothing and returns 0 if routable. Otherwise prints ONE reason on
# stdout, the way sot_inbox_append does, for the caller to print after its
# own `FAILED` prefix, and returns 1. An unreadable registry is its own
# reason, never "no registry row". Depends on NAME/PROJECT_ROOT already being set by
# `eval "$(comm-context.sh)"` — call after that, never before.
sot_require_routable_identity() {
    local why
    why="$(sot_require_agent)" || { echo "$why"; return 1; }
    _sot_identity_routable
}
# _sot_identity_routable — the rest of the above, for a caller that has already
# passed sot_require_agent (comm-send.sh gates first, so it walks once).
_sot_identity_routable() {
    if [ -z "${NAME:-}" ]; then
        echo "your sot-comm identity did not resolve — refusing to send with no verifiable from-handle (a reply would silently misroute). Join first: comm-join.sh --name <canonical-handle> (never a bare comm-join.sh if you previously held one — see the sot-session-start skill's recovery recipe)."
        return 1
    fi
    local reg_status reg_root qname qdir
    IFS=$'\t' read -r reg_status reg_root <<< "$(sot_registry_entry_status "$NAME")"
    # %q-quote the handle AND the executable path (Codex review round-3
    # finding 7): a raw $NAME/$SCRIPT_DIR interpolation produces a wrong
    # or unsafe copy-paste command for a handle or install path containing
    # spaces/metacharacters. $SCRIPT_DIR is this HELPER's caller's own
    # directory (every caller sources comm-lib.sh after setting it).
    qname="$(printf '%q' "$NAME")"
    qdir="$(printf '%q' "${SCRIPT_DIR:-.}")"
    case "$reg_status" in
        present) ;;
        absent) echo "your identity @$NAME has no registry row; reclaim it with $qdir/comm-join.sh --name $qname"; return 1 ;;
        *) echo "the registry could not be read, so identity @$NAME is unverified; nothing was sent"; return 1 ;;
    esac
    if [ -n "$reg_root" ] && [ "$reg_root" != "${PROJECT_ROOT:-}" ]; then
        echo "your sot-comm identity '@$NAME' is registered to a DIFFERENT project's root ('$reg_root') — refusing to send with a misrouting from-handle. Reclaim it: $qdir/comm-join.sh --name $qname"
        return 1
    fi
    return 0
}

# --- derived-handle disambiguation (ADR 0028 addendum: "derived vs
# explicit") --- single home for the algorithm; comm-join.sh and
# comm-spawn.sh both call sot_derive_handle. This is ONLY for a name that
# comes from DERIVATION (the default <basename>-<host>): a caller must never
# route an explicit --name, $SOT_COMM_NAME, or an already-joined self-file
# identity through here — those stay verbatim, unconditionally.

# sot_canonical_path PATH — absolute, symlink-resolved path (the
# "canonical project root" the disambiguation compares), or NOTHING on
# stdout plus a nonzero return if one can't be established (Codex review
# F8). NEVER falls back to an unresolved/relative path: two callers that
# each `cd` into a differently-spelled relative path (or the SAME literal
# "./foo" from two different directories) would otherwise compare as
# identical roots, defeating the whole disambiguation.
sot_canonical_path() {
    local p="$1" out
    if command -v realpath >/dev/null 2>&1; then
        if out="$(realpath -- "$p" 2>/dev/null)" && [ -n "$out" ]; then
            printf '%s\n' "$out"
            return 0
        fi
    fi
    if command -v readlink >/dev/null 2>&1; then
        if out="$(readlink -f -- "$p" 2>/dev/null)" && [ -n "$out" ]; then
            printf '%s\n' "$out"
            return 0
        fi
    fi
    echo "sot_canonical_path: could not resolve a canonical path for '$p' (no working realpath or readlink -f) — refusing to record a relative/unresolved project root" >&2
    return 1
}

# sot_hash6 STR — first 6 hex chars of sha256(STR); stable per input, used
# only for the last-resort hash-qualified handle tier. NO fallback when
# sha256sum/shasum are both missing (Codex review F8 / simplicity audit):
# the earlier `cksum` fallback was a variable-length decimal CRC, not a
# hash6 — installing sha256sum later would silently change that root's
# tier-3 handle. Fail loudly instead; the caller surfaces this as a hard
# error asking for an explicit --name.
#
# Both the pipeline's exit status AND the shape of its output are checked
# (Codex review PR #148 round 2, finding 5): the previous version ran
# `return 0` unconditionally after each pipeline, so an INSTALLED-but-
# FAILING sha256sum (confirmed: one that exits 23) still "succeeded" with
# an EMPTY hash, producing a `<base>--<host>` handle instead of a loud
# failure. `rc=$?` right after the pipeline reflects its real exit status
# under this shell's `pipefail` (both callers set it); the regex is the
# stronger, direct check — it also catches a tool that exits 0 but emits
# garbage, which an exit-code check alone would miss.
sot_hash6() {
    local out rc
    if command -v sha256sum >/dev/null 2>&1; then
        out="$(printf '%s' "$1" | sha256sum | cut -c1-6)"; rc=$?
        if [ "$rc" -eq 0 ] && [[ "$out" =~ ^[0-9a-f]{6}$ ]]; then
            printf '%s\n' "$out"
            return 0
        fi
    fi
    if command -v shasum >/dev/null 2>&1; then
        out="$(printf '%s' "$1" | shasum -a 256 | cut -c1-6)"; rc=$?
        if [ "$rc" -eq 0 ] && [[ "$out" =~ ^[0-9a-f]{6}$ ]]; then
            printf '%s\n' "$out"
            return 0
        fi
    fi
    echo "sot_hash6: no working sha256sum/shasum produced a valid 6-hex-character digest — cannot compute a stable tier-3 handle qualifier" >&2
    return 1
}

# sot_slug LABEL — bash mirror of sot_protocol::slug (Codex
# review PR #148 round 2, finding 3): lowercase; '.' -> '_' BEFORE the
# keep-check; a RUN of characters outside [a-z0-9_-] collapses to a single
# '-' (a LITERAL '-'/'_'/alnum in the input is pushed as-is and never
# collapsed, even if repeated — matching Rust's keep/else branch split
# exactly, not a blanket dash-collapse); trailing '-' trimmed; empty ->
# "default". Verified against every example in that function's own doc
# comment (MyPackage.jl -> mypackage_jl, "Foo Bar" -> foo-bar, /abs/path
# -> abs-path, "  " -> default) plus literal-repeated-dash and leading-
# junk cases. Needed because workspace.create's same-slug path
# refreshes a row that is not in use instead of refusing — two labels
# that only differ by case, or by a dot vs underscore, resolve to the
# SAME workspace and must be caught as a collision too, not just a
# byte-identical label match.
sot_slug() {
    local label="$1" out="" last_dash=false i len ch c
    len=${#label}
    for (( i = 0; i < len; i++ )); do
        ch="${label:i:1}"
        c="$(printf '%s' "$ch" | tr '[:upper:]' '[:lower:]')"
        [ "$c" = "." ] && c="_"
        case "$c" in
            [a-z0-9_-])
                out="${out}${c}"
                if [ "$c" = "-" ]; then last_dash=true; else last_dash=false; fi
                ;;
            *)
                if [ "$last_dash" = false ] && [ -n "$out" ]; then
                    out="${out}-"
                    last_dash=true
                fi
                ;;
        esac
    done
    while [[ "$out" == *- ]]; do out="${out%-}"; done
    [ -z "$out" ] && out="default"
    printf '%s\n' "$out"
}

# sot_sanitize_component STR [MAXLEN=20] — reduce STR to the
# workspace.create charset [A-Za-z0-9._-] (every other byte becomes '-',
# runs of '-' collapse, leading/trailing '-' trimmed) and clamp it to
# MAXLEN (Codex review F4): a repo/parentdir basename can contain spaces,
# Unicode, or shell metacharacters, none of which workspace.create's name
# validator (`rust/backend/src/paths.rs`, `valid_name`) accepts — and an
# unsanitized basename reaching the `--no-workspace` launcher string is a
# shell-injection vector. Applied to EVERY raw piece (basename, parentdir,
# host) BEFORE composing a candidate, never to the assembled candidate
# afterward, so composed separators can't be reintroduced or hidden behind
# a length overflow. MAXLEN defaults to 20 so the worst-case composed
# candidate (20 + "-" + 20 + "-" + 20 = 62) stays inside workspace.create's
# 64-char limit without per-tier budget arithmetic.
sot_sanitize_component() {
    local s="$1" max="${2:-20}"
    s="$(printf '%s' "$s" | tr -c 'A-Za-z0-9._-' '-')"
    while [[ "$s" == *--* ]]; do s="${s//--/-}"; done
    s="${s#-}"; s="${s%-}"
    s="${s:0:$max}"
    s="${s%-}"
    [ -z "$s" ] && s="x"
    printf '%s' "$s"
}

# _sot_tier_claimable MODE ROOT STATUS HELD_ROOT — true if a tier whose
# registry status is STATUS/HELD_ROOT (from sot_registry_entry_status) can
# be claimed under MODE:
#   reclaim — unclaimed, OR already held by MY OWN root (today's
#             comm-join rejoin/reclaim behavior).
#   fresh   — unclaimed ONLY. comm-spawn creates a NEW agent; an existing
#             row for the resolved name — even one sharing my root — is
#             someone/something else's from spawn's point of view and must
#             never be silently absorbed (Codex review F3: this used to
#             erase a LIVE agent's workspace_id/status fields when spawning a
#             second time against the same project root).
_sot_tier_claimable() {
    local mode="$1" root="$2" status="$3" held="$4"
    [ "$status" = "absent" ] && return 0
    [ "$mode" = "reclaim" ] && [ "$held" = "$root" ] && return 0
    return 1
}

# sot_derive_handle MODE ROOT HOST — the derived-name algorithm (ADR 0028
# addendum; see docs/adr/0028-remote-comm-autoconnect.md). MODE is
# "reclaim" (comm-join) or "fresh" (comm-spawn) — see _sot_tier_claimable.
# Liveness is deliberately NOT consulted — a stale registry row still
# holds its claim until existing cleanup paths remove it; this keeps the
# rule one-dimensional (root comparison only) and prevents handle
# flip-flop between two projects depending on who happens to be running.
#
# ROOT MUST already be canonical (sot_canonical_path) — canonicalizing
# here would mean filesystem traversal INSIDE the registry lock (this runs
# from claim_derived_handle, under with_lock), which is worse under a
# dead/slow NFS mount and was flagged as redundant (Codex review F8:
# "canonicalize once before entering the lock").
#
# Escalates through three tiers, each checked with _sot_tier_claimable —
# tier 3 is NOT an unconditional overwrite (Codex review F6: it used to be
# claimed regardless of who held it, which needs no hash collision at all
# to hit — an explicit owner of the computed hash-qualified name was
# silently overwritten). If no tier is claimable, this FAILS LOUDLY
# (nonzero return, nothing on stdout, a clear reason on stderr) rather than
# inventing a fourth tier or overwriting anything — the caller must ask
# the user for an explicit --name.
#
# Every raw piece (repo basename, parentdir, host) is sanitized+clamped
# (sot_sanitize_component) BEFORE composing any candidate (Codex review
# F4), so a derived handle can never diverge from what workspace.create
# will accept, and no raw path text reaches a shell command unsanitized.
# HOST gets an extra step (Codex review PR #148 round 2, finding 7): if
# sanitizing/clamping CHANGES it at all — a long or characters-outside-
# charset hostname got truncated/rewritten — a short digest of the RAW
# host is appended. Without this, two DIFFERENT real hosts whose names
# happen to sanitize/truncate to the IDENTICAL string would, if they ever
# shared a root (an NFS-shared repo, exactly this cluster's own shape),
# alias onto one tier-1 "reclaim" — root matches, and the (now-identical)
# host component can no longer tell them apart. An untouched host (the
# overwhelmingly common case: short, already-valid hostnames) gets no
# suffix, so today's handles are unchanged. HOST_RAW_MAX=12 leaves room
# for "-" + a 6-hex digest without the host component threatening
# sot_sanitize_component's 20-char default budget the other components
# still use (12 + 1 + 6 = 19 worst case).
#
# On success, prints THREE lines: handle, qualifier, tier1 (qualifier empty
# at tier 1, "<parentdir>" at tier 2, "<hash6>" at tier 3; tier1 is ALWAYS
# the bare "<base>-<host>" handle, win or lose, so a caller can tell whether
# this call escalated AWAY from it — comm-join.sh's stranding guard needs
# exactly that). NEWLINE-separated, not tab-separated (Codex review round-1
# finding 1): tab is one of bash's IFS-WHITESPACE characters, so `IFS=$'\t'
# read -r a b c` still COLLAPSES adjacent tabs exactly like the default
# space/tab/newline splitting does — an empty qualifier field (the tier-1
# case, `tier1<TAB><TAB>tier1`) vanished entirely instead of reading back as
# "", shifting tier1's value into qualifier and leaving CLAIMED_TIER1 empty.
# Every ordinary tier-1 spawn then misread CLAIMED_QUALIFIER as non-empty
# and comm-spawn.sh synthesized a wrong "qualified" display label. Reading
# one line per `read -r VAR` sidesteps this: with a single destination
# variable there is no splitting to collapse — the whole line, empty or
# not, becomes that variable's value verbatim. A caller that only wants the
# handle: `NAME="$(sot_derive_handle reclaim "$ROOT" "$HOST" | head -n1)"`.
sot_derive_handle() {
    local mode="$1" root="$2" raw_host="$3"
    local base parent hash6 tier1 tier2 tier3 host host_digest
    local status1 held1 status2 held2 status3 held3 shown1 shown2 shown3
    local unreadable="comm: the registry could not be read, so no handle was derived; nothing was written"

    case "$mode" in
        reclaim|fresh) : ;;
        *) echo "sot_derive_handle: invalid mode '$mode' (want reclaim or fresh)" >&2; return 1 ;;
    esac

    base="$(sot_sanitize_component "$(basename "$root")")"
    host="$(sot_sanitize_component "$raw_host" 12)"
    if [ "$host" != "$raw_host" ]; then
        host_digest="$(sot_hash6 "$raw_host")" || return 1
        host="${host}-${host_digest}"
    fi

    tier1="${base}-${host}"
    IFS=$'\t' read -r status1 held1 <<< "$(sot_registry_entry_status "$tier1")"
    [ "$status1" = "error" ] && { echo "$unreadable" >&2; return 1; }
    if _sot_tier_claimable "$mode" "$root" "$status1" "$held1"; then
        printf '%s\n%s\n%s\n' "$tier1" "" "$tier1"
        return 0
    fi
    shown1="$held1"; [ -z "$shown1" ] && shown1="an unknown project"

    parent="$(sot_sanitize_component "$(basename "$(dirname "$root")")")"
    tier2="${base}-${parent}-${host}"
    IFS=$'\t' read -r status2 held2 <<< "$(sot_registry_entry_status "$tier2")"
    [ "$status2" = "error" ] && { echo "$unreadable" >&2; return 1; }
    if _sot_tier_claimable "$mode" "$root" "$status2" "$held2"; then
        echo "comm: '@$tier1' is already held by $shown1 — joining as '@$tier2' instead" >&2
        printf '%s\n%s\n%s\n' "$tier2" "$parent" "$tier1"
        return 0
    fi
    shown2="$held2"; [ -z "$shown2" ] && shown2="an unknown project"

    hash6="$(sot_hash6 "$root")" || return 1
    tier3="${base}-${hash6}-${host}"
    IFS=$'\t' read -r status3 held3 <<< "$(sot_registry_entry_status "$tier3")"
    [ "$status3" = "error" ] && { echo "$unreadable" >&2; return 1; }
    if _sot_tier_claimable "$mode" "$root" "$status3" "$held3"; then
        echo "comm: '@$tier1' (held by $shown1) and '@$tier2' (held by $shown2) are both taken — joining as '@$tier3' instead" >&2
        printf '%s\n%s\n%s\n' "$tier3" "$hash6" "$tier1"
        return 0
    fi
    shown3="$held3"; [ -z "$shown3" ] && shown3="an unknown project"
    echo "comm: every derived handle for this project is already taken — '@$tier1' (held by $shown1), '@$tier2' (held by $shown2), and '@$tier3' (held by $shown3). Pass --name to pick one explicitly." >&2
    return 1
}

# --- atomic derive + claim (closes the read-then-write race) -----------
# sot_derive_handle above only DECIDES a name by reading the registry; a
# caller that derives and only LATER locks to registry_put leaves a window
# between the two where a second, concurrent derived join (a DIFFERENT
# root, same basename+host) can observe that exact same "still free" state
# and also decide on tier 1 — whichever registry_put lands second then
# silently clobbers the first. That is the aliasing bug this whole feature
# exists to close, so it must not survive as a race window. The window is
# not theoretical: comm-spawn.sh is driven programmatically for bulk
# workspace bring-up, joining many sessions back-to-back.
#
# claim_derived_handle MODE ROOT HOST OBJ_JSON — derive AND registry_put
# the result as ONE critical section under the registry lock, so no other
# claim can observe registry state in between. Sets globals CLAIMED_NAME,
# CLAIMED_QUALIFIER, and CLAIMED_TIER1 (mirrors sot_derive_handle's three
# outputs) for the caller to read after this returns; all three cleared to
# "" first, so a failure never leaves a stale value from a PREVIOUS
# successful call for a careless caller to read. CLAIMED_TIER1 is what
# comm-join.sh's stranding guard compares CLAIMED_NAME against — a mismatch
# means this call escalated away from the bare handle. Both comm-join.sh
# (MODE reclaim) and
# comm-spawn.sh (MODE fresh, the provisional row) route a derived name
# through this — one shared locked claim path, not two copies of "derive,
# then lock to write" that could each get this wrong.
#
# On failure (sot_derive_handle exhausted all three tiers, or refused to
# run at all — e.g. no hash function available), returns nonzero and
# writes NOTHING to the registry (Codex review F7): derivation failure is
# an error the caller must surface, never a claim of "".
#
# _sot_claim_derived_handle is the with_lock callee; it must NEVER be
# invoked directly, and never as `X=$(with_lock ...)`. with_lock runs its
# command directly ("$@", no subshell) specifically so a callee's global
# assignment survives past its return — capturing it via command
# substitution would fork a subshell and lose CLAIMED_NAME exactly the way
# the test harness's own next_self_file() lost its counter (see
# comm/tests/test-join-disambiguation.sh) — the same lesson, twice.
_sot_claim_derived_handle() {  # MODE ROOT HOST OBJ_JSON — call only via with_lock
    local mode="$1" root="$2" host="$3" obj="$4" line
    CLAIMED_NAME=""
    CLAIMED_QUALIFIER=""
    CLAIMED_TIER1=""
    line="$(sot_derive_handle "$mode" "$root" "$host")" || return 1
    # Three sequential single-var reads off the SAME herestring fd (Codex
    # review round-1 finding 1) — each `read -r VAR` consumes one line and
    # advances the shared position, and with only one destination variable
    # there is no IFS splitting to collapse an empty middle field the way
    # the old tab-delimited `read -r a b c` did. `{ …; } <<< "$line"` (not
    # `( … )`) so the reads run in THIS shell and CLAIMED_* stay set for the
    # caller.
    {
        IFS= read -r CLAIMED_NAME
        IFS= read -r CLAIMED_QUALIFIER
        IFS= read -r CLAIMED_TIER1
    } <<< "$line"
    if [ -z "$CLAIMED_NAME" ]; then
        echo "claim_derived_handle: derivation returned no name — refusing to claim an empty handle" >&2
        return 1
    fi
    registry_put "$CLAIMED_NAME" "$obj"
}
claim_derived_handle() {  # MODE ROOT HOST OBJ_JSON
    with_lock _sot_claim_derived_handle "$1" "$2" "$3" "$4"
}


