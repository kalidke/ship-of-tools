#!/usr/bin/env bash
# test-join-disambiguation.sh — self-contained test for the derived-handle
# disambiguation feature (ADR 0028 addendum: "derived vs explicit"). No bats
# dependency. HERMETIC: runs against a temp $SOT_COMM_HOME, a temp self-file
# per simulated session (via $SOT_COMM_SELF_FILE), a PINNED host (via
# $SOT_COMM_TEST_HOST — see below), and an isolated tmux server (via
# $SOT_TMUX_SOCK) for the comm-spawn.sh case — never touches the real
# ~/.sot-comm or the real per-user tmux socket (a comm-spawn.sh smoke run
# during this feature's development that omitted the tmux isolation created
# real stray sessions on the shared production socket; every
# spawn-exercising case here sets it).
#
# HOST must be hermetic too, not just HOME (CI incident): this script used
# to build its EXPECTED handles from the real `hostname -s`. That's fine on
# a short-hostnamed dev box, but a CI runner's hostname can be long enough
# to trip sot_derive_handle's F7 host-alias guard (comm-lib.sh) — the guard
# then appends a digest suffix the test's naively-built expectation didn't
# account for, and every case asserting a DERIVED handle mismatches (8/13
# failed this way on GitHub Actions while 13/13 passed locally). Pinning
# HOST through $SOT_COMM_TEST_LOCK_BARRIER's sibling seam,
# $SOT_COMM_TEST_HOST (comm-context.sh), removes the dependency on both
# sides — the scripts' actual host and this test's expected-handle host are
# now the SAME fixed, short, already-clean string, regardless of what box
# runs the suite. case_host_alias_guard_triggers_on_long_host below
# separately routes a deliberately long/dirty host through the SAME seam
# for ONE case, so the guard itself still gets positive coverage rather
# than being dodged everywhere.
#
# case_lock_closes_derive_write_gap also covers claim_derived_handle's
# atomicity (derive + registry_put as one locked step): it deterministically
# interleaves a registry mutation into a backgrounded derived join's
# wait-for-lock window, synchronized via with_lock's own
# $SOT_COMM_TEST_LOCK_BARRIER test seam (a file touched right before its
# first mkdir attempt) rather than a sleep, with bounded waits throughout so
# a genuinely stuck child fails the test instead of hanging it.
#
# Usage: comm/core/tests/test-join-disambiguation.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/../scripts" && pwd)"
JOIN="$SCRIPTS_DIR/comm-join.sh"
SPAWN="$SCRIPTS_DIR/comm-spawn.sh"
CONTEXT="$SCRIPTS_DIR/comm-context.sh"
SEND="$SCRIPTS_DIR/comm-send.sh"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-comm-test-XXXXXX")"
# Codex review (PR #148, test notes): an unchecked mktemp failure leaves
# WORK="", and every "$WORK/..." path below silently becomes an absolute
# path rooted at "/" (e.g. "$WORK/home" -> "/home") — checked explicitly,
# not just via `|| exit`, since a bizarre mktemp could exit 0 with empty
# stdout too.
if [ -z "$WORK" ] || [ ! -d "$WORK" ]; then
    echo "FATAL: mktemp did not produce a usable work directory (got: '$WORK')" >&2
    exit 1
fi

export SOT_COMM_HOME="$WORK/home"
mkdir -p "$SOT_COMM_HOME"
REGISTRY="$SOT_COMM_HOME/registry.json"
# Sourced (not just invoked as external scripts, like comm-join.sh/
# comm-spawn.sh below) so a few cases can call comm-lib.sh primitives
# directly — with_lock, registry_put, registry_del_if_provisional — for
# focused coverage of mechanisms that are awkward to drive end-to-end
# (the rollback-ownership and trap-restore cases, round 2 finding 10).
# Safe to source here: it only sets variables/functions from
# $SOT_COMM_HOME, already exported above.
source "$SCRIPTS_DIR/comm-lib.sh"
ensure_home
# Mirrors comm-lib.sh's LOCKDIR="$COMM_HOME/.registry.lock" exactly — used by
# case_lock_closes_derive_write_gap below to simulate a concurrent claim
# landing WHILE a derived join is blocked waiting for the lock.
LOCKDIR="$SOT_COMM_HOME/.registry.lock"
# Isolated tmux server for comm-spawn.sh's --no-workspace path
# (case_spawn_fresh_only_refusal) — exported globally so every comm-spawn.sh
# invocation in this file picks it up; comm-join.sh never touches tmux, so
# this is a harmless no-op for every other case.
export SOT_TMUX_SOCK="$WORK/tmux.sock"
trap 'tmux -S "$SOT_TMUX_SOCK" kill-server >/dev/null 2>&1 || true; rm -rf "$WORK"' EXIT

# Pinned, hermetic HOST — see the file header. Deliberately short and
# already within the allowed charset so it is NEVER transformed by
# sot_sanitize_component/the F7 host-alias guard: every case except
# case_host_alias_guard_triggers_on_long_host expects an UNTRANSFORMED
# host in its derived handles, and this value must hold that invariant
# regardless of what machine or CI runner executes this script.
HOST="testhost"
export SOT_COMM_TEST_HOST="$HOST"

PASS=0
FAIL=0

# check DESC FN — run FN (which prints diagnostics and returns 1 on
# failure), then print the required PASS/FAIL line.
check() {
    local desc="$1" fn="$2"
    if "$fn"; then
        echo "PASS: $desc"
        PASS=$((PASS + 1))
    else
        echo "FAIL: $desc"
        FAIL=$((FAIL + 1))
    fi
}

# --- fake project roots -------------------------------------------------
# Three roots share the leaf basename "instructor-materials". The first two
# ALSO share the parent basename "groupX" — set up on purpose so the third
# forces the three-way (hash-tier) collision: tier1 (bare) and tier2
# (parentdir-qualified) are both already taken by the time it joins.
mkdir -p "$WORK/site1/groupX/instructor-materials"
mkdir -p "$WORK/site2/groupX/instructor-materials"
mkdir -p "$WORK/site3/groupX/instructor-materials"
mkdir -p "$WORK/other-repo"
mkdir -p "$WORK/other-repo2"

ROOT1="$(realpath "$WORK/site1/groupX/instructor-materials")"
ROOT2="$(realpath "$WORK/site2/groupX/instructor-materials")"
ROOT3="$(realpath "$WORK/site3/groupX/instructor-materials")"
ROOT4="$(realpath "$WORK/other-repo")"
ROOT5="$(realpath "$WORK/other-repo2")"

BASE="instructor-materials"
H1="${BASE}-${HOST}"                  # tier 1: bare
H2="${BASE}-groupX-${HOST}"           # tier 2: parentdir-qualified

# --- helpers -------------------------------------------------------------

# next_self_file — sets $NEXT_SELF_FILE to a fresh, never-before-used path.
# NOT a `$(...)` command-substitution helper on purpose: that would fork a
# subshell, and the SELFN increment would be lost on return (every call
# would hand back "self-1.txt") — the exact bug this comment now guards
# against, caught by this test failing against itself during development.
SELFN=0
NEXT_SELF_FILE=""
next_self_file() {
    SELFN=$((SELFN + 1))
    NEXT_SELF_FILE="$WORK/self-$SELFN.txt"
}

# join_in ROOT [ARGS...] — run comm-join.sh with cwd=ROOT and a FRESH,
# never-before-seen self-file, so every call simulates a brand-new session
# (no inherited identity) unless ARGS/env explicitly supply one. Sets
# JOIN_OUT / JOIN_ERR / JOIN_RC. JOIN_ENV_NAME, when non-empty, is exported
# as $SOT_COMM_NAME for that one call (used by the env-verbatim case).
# JOIN_SELF_FILE_OVERRIDE, when non-empty, pins the self-file to an
# EXISTING crafted path instead of a fresh one (used by the self-file
# root-validation case, which needs to pre-populate the file's contents).
# JOIN_PATH_PREFIX, when non-empty, is prepended to $PATH for that one
# call (used by the hash-failure case to put a fake, failing sha256sum
# ahead of the real one).
JOIN_OUT=""; JOIN_ERR=""; JOIN_RC=0
JOIN_ENV_NAME=""
JOIN_SELF_FILE_OVERRIDE=""
JOIN_PATH_PREFIX=""
join_in() {
    local root="$1"; shift
    local self errfile path_arg
    if [ -n "$JOIN_SELF_FILE_OVERRIDE" ]; then
        self="$JOIN_SELF_FILE_OVERRIDE"
    else
        next_self_file
        self="$NEXT_SELF_FILE"
    fi
    errfile="$WORK/stderr.tmp"
    path_arg="${JOIN_PATH_PREFIX:+$JOIN_PATH_PREFIX:}$PATH"
    JOIN_OUT="$(cd "$root" && PATH="$path_arg" SOT_COMM_SELF_FILE="$self" SOT_COMM_NAME="$JOIN_ENV_NAME" \
        "$JOIN" "$@" 2>"$errfile")"
    JOIN_RC=$?
    JOIN_ERR="$(cat "$errfile" 2>/dev/null || true)"
}

# spawn_in ROOT [ARGS...] — run comm-spawn.sh in --no-workspace mode (no
# daemon needed) against ROOT. Sets SPAWN_OUT / SPAWN_ERR / SPAWN_RC.
SPAWN_OUT=""; SPAWN_ERR=""; SPAWN_RC=0
spawn_in() {
    local root="$1"; shift
    local errfile="$WORK/spawn-stderr.tmp"
    SPAWN_OUT="$("$SPAWN" "$root" --no-workspace "$@" 2>"$errfile")"
    SPAWN_RC=$?
    SPAWN_ERR="$(cat "$errfile" 2>/dev/null || true)"
}

# context_in ROOT SELF — run comm-context.sh DIRECTLY (not through
# comm-join.sh) with cwd=ROOT and self-file SELF, which may pre-exist
# (crafted by the caller) — for cases that test comm-context.sh's own
# self-file validation/self-heal in isolation, with no registry side
# effects at all. Sets CTX_NAME (scraped from the NAME= line of its
# eval-able output — safe here since every handle this suite uses is plain
# ASCII with no characters %q would ever quote) / CTX_OUT / CTX_ERR / CTX_RC.
CTX_NAME=""; CTX_OUT=""; CTX_ERR=""; CTX_RC=0
context_in() {
    local root="$1" self="$2"
    local errfile="$WORK/context-stderr.tmp"
    CTX_OUT="$(cd "$root" && SOT_COMM_SELF_FILE="$self" SOT_COMM_TEST_HOST="$HOST" "$CONTEXT" 2>"$errfile")"
    CTX_RC=$?
    CTX_ERR="$(cat "$errfile" 2>/dev/null || true)"
    CTX_NAME="$(printf '%s\n' "$CTX_OUT" | sed -n 's/^NAME=//p')"
}

registry_root() {  # NAME -> prints its `root`, or MISSING if unset/absent
    jq -r --arg n "$1" '.agents[$n].root // "MISSING"' "$REGISTRY" 2>/dev/null
}
registry_field() {  # NAME FIELD -> prints the field, or MISSING if unset/absent
    jq -r --arg n "$1" --arg f "$2" '.agents[$n][$f] // "MISSING"' "$REGISTRY" 2>/dev/null
}
registry_has_root_key() {  # NAME -> "yes" if the row has a `root` KEY at all (even ""), "no" otherwise
    jq -r --arg n "$1" 'if (.agents[$n] // {}) | has("root") then "yes" else "no" end' "$REGISTRY" 2>/dev/null
}

contains() { case "$1" in *"$2"*) return 0 ;; *) return 1 ;; esac; }

# --- cases -----------------------------------------------------------

case_fresh_claim() {
    join_in "$ROOT1"
    [ "$JOIN_RC" -eq 0 ] || { echo "  comm-join.sh exited $JOIN_RC: $JOIN_ERR"; return 1; }
    contains "$JOIN_OUT" "Joined sot-comm as @$H1" || { echo "  stdout: $JOIN_OUT"; return 1; }
    [ "$(registry_root "$H1")" = "$ROOT1" ] || { echo "  root=$(registry_root "$H1"), want $ROOT1"; return 1; }
    return 0
}

case_same_root_rejoin() {
    # A second, independent "session" (fresh self-file — simulates a new
    # pane, not a literal rejoin) joining from the SAME root: today's
    # reclaim behavior must be unchanged — same bare handle, no escalation.
    join_in "$ROOT1"
    [ "$JOIN_RC" -eq 0 ] || { echo "  comm-join.sh exited $JOIN_RC: $JOIN_ERR"; return 1; }
    contains "$JOIN_OUT" "Joined sot-comm as @$H1" || { echo "  stdout: $JOIN_OUT"; return 1; }
    [ "$(registry_root "$H1")" = "$ROOT1" ] || { echo "  root changed: $(registry_root "$H1")"; return 1; }
    contains "$JOIN_ERR" "already held" && { echo "  unexpected qualification notice: $JOIN_ERR"; return 1; }
    return 0
}

case_diff_root_collision() {
    join_in "$ROOT2"
    [ "$JOIN_RC" -eq 0 ] || { echo "  comm-join.sh exited $JOIN_RC: $JOIN_ERR"; return 1; }
    contains "$JOIN_OUT" "Joined sot-comm as @$H2" || { echo "  stdout: $JOIN_OUT (want @$H2)"; return 1; }
    [ "$(registry_root "$H2")" = "$ROOT2" ] || { echo "  H2 root=$(registry_root "$H2"), want $ROOT2"; return 1; }
    # the first entry must be untouched by the second session's escalation
    [ "$(registry_root "$H1")" = "$ROOT1" ] || { echo "  H1 (first entry) was mutated: $(registry_root "$H1")"; return 1; }
    contains "$JOIN_ERR" "$H1" || { echo "  stderr missing bare-handle notice: $JOIN_ERR"; return 1; }
    contains "$JOIN_ERR" "$H2" || { echo "  stderr missing qualified-handle notice: $JOIN_ERR"; return 1; }
    return 0
}

case_three_way_collision() {
    local hash6 h3
    hash6="$(printf '%s' "$ROOT3" | sha256sum | cut -c1-6)"
    h3="${BASE}-${hash6}-${HOST}"
    join_in "$ROOT3"
    [ "$JOIN_RC" -eq 0 ] || { echo "  comm-join.sh exited $JOIN_RC: $JOIN_ERR"; return 1; }
    contains "$JOIN_OUT" "Joined sot-comm as @$h3" || { echo "  stdout: $JOIN_OUT (want @$h3)"; return 1; }
    [ "$(registry_root "$h3")" = "$ROOT3" ] || { echo "  h3 root=$(registry_root "$h3"), want $ROOT3"; return 1; }
    [ "$(registry_root "$H1")" = "$ROOT1" ] || { echo "  H1 was mutated: $(registry_root "$H1")"; return 1; }
    [ "$(registry_root "$H2")" = "$ROOT2" ] || { echo "  H2 was mutated: $(registry_root "$H2")"; return 1; }
    return 0
}

case_explicit_name_verbatim() {
    # Explicitly claiming the ALREADY-HELD bare handle from a totally
    # different (4th) root must be used verbatim — no auto-disambiguation,
    # today's overwrite behavior, no qualification notice.
    join_in "$ROOT4" --name "$H1"
    [ "$JOIN_RC" -eq 0 ] || { echo "  comm-join.sh exited $JOIN_RC: $JOIN_ERR"; return 1; }
    contains "$JOIN_OUT" "Joined sot-comm as @$H1" || { echo "  stdout: $JOIN_OUT"; return 1; }
    [ "$(registry_root "$H1")" = "$ROOT4" ] || { echo "  expected overwrite to $ROOT4, got $(registry_root "$H1")"; return 1; }
    contains "$JOIN_ERR" "already held" && { echo "  unexpected disambiguation on explicit --name: $JOIN_ERR"; return 1; }
    return 0
}

case_env_name_verbatim() {
    # Same as above but via $SOT_COMM_NAME instead of --name, colliding with
    # the tier-2 handle claimed earlier.
    JOIN_ENV_NAME="$H2"
    join_in "$ROOT5"
    JOIN_ENV_NAME=""
    [ "$JOIN_RC" -eq 0 ] || { echo "  comm-join.sh exited $JOIN_RC: $JOIN_ERR"; return 1; }
    contains "$JOIN_OUT" "Joined sot-comm as @$H2" || { echo "  stdout: $JOIN_OUT"; return 1; }
    [ "$(registry_root "$H2")" = "$ROOT5" ] || { echo "  expected overwrite to $ROOT5, got $(registry_root "$H2")"; return 1; }
    contains "$JOIN_ERR" "already held" && { echo "  unexpected disambiguation via \$SOT_COMM_NAME: $JOIN_ERR"; return 1; }
    return 0
}

case_legacy_matching_self_file_is_reclaimed_not_rederived() {
    # Field regression fix (coordinator ruling, item 1). This case used to
    # assert the OPPOSITE of what follows: that a legacy (pre-#148, no
    # root=) self-file whose repo= line matches this project must be
    # discarded and forced through fresh derivation. That was exactly the
    # bug: PR #148 shipped that as unconditional, and EVERY self-file
    # written before it (the overwhelming majority in the field) lacks
    # root= — evicting every long-running session from its own identity on
    # its next comm call. A legacy self-file whose repo= matches must
    # instead be ACCEPTED and reclaimed verbatim (never re-derived), with
    # root= self-healed onto it so every subsequent read is fully
    # validated. See case_v2_self_file_wrong_root_is_still_discarded below
    # for the strict root= check this does NOT relax.
    local root base h1 crafted
    mkdir -p "$WORK/selftest/proj2"
    root="$(realpath "$WORK/selftest/proj2")"
    base="proj2"
    h1="${base}-${HOST}"

    crafted="$WORK/crafted-self.txt"
    # A LEGACY two-line self-file (repo= but no root=). The point of this
    # test is that comm-join.sh must NOT re-derive at all — it must reclaim
    # 'legacy-claimed-name' verbatim, not land on the freshly-derived $h1.
    printf 'legacy-claimed-name\nrepo=%s\n' "$base" > "$crafted"

    JOIN_SELF_FILE_OVERRIDE="$crafted"
    join_in "$root"
    JOIN_SELF_FILE_OVERRIDE=""

    [ "$JOIN_RC" -eq 0 ] || { echo "  exited $JOIN_RC: $JOIN_ERR"; return 1; }
    contains "$JOIN_ERR" "stale" && { echo "  unexpected staleness notice for a matching legacy self-file: $JOIN_ERR"; return 1; }
    contains "$JOIN_ERR" "self-healed" || { echo "  missing self-heal notice: $JOIN_ERR"; return 1; }
    contains "$JOIN_OUT" "Joined sot-comm as @legacy-claimed-name" \
        || { echo "  stdout: $JOIN_OUT (want the pre-existing identity 'legacy-claimed-name' reclaimed, NOT fresh derivation to @$h1)"; return 1; }
    [ "$(registry_root "legacy-claimed-name")" = "$root" ] \
        || { echo "  root=$(registry_root "legacy-claimed-name"), want $root"; return 1; }
    [ "$(registry_root "$h1")" = "MISSING" ] \
        || { echo "  fresh derivation ALSO ran and claimed @$h1: $(registry_root "$h1")"; return 1; }

    # The self-file must now have been rewritten to full v2 (root= present).
    local lines; lines="$(wc -l < "$crafted")"
    [ "$lines" -ge 3 ] || { echo "  self-file not upgraded to v2 with root=: $(cat "$crafted")"; return 1; }
    local backfilled_root; backfilled_root="$(sed -n '3p' "$crafted" | sed -n 's/^root=//p')"
    [ "$backfilled_root" = "$root" ] || { echo "  backfilled root=$backfilled_root, want $root"; return 1; }
    return 0
}

case_legacy_self_file_matching_repo_accepted_and_backfilled() {
    # (a) Direct comm-context.sh unit coverage of the same fix, with no
    # registry side effects: a legacy self-file (no root=) whose repo=
    # matches is accepted verbatim and self-healed on read, and does NOT
    # re-heal (or re-notify) on a second read once it's already v2.
    local root base self
    mkdir -p "$WORK/legacy-accept/proj4"
    root="$(realpath "$WORK/legacy-accept/proj4")"
    base="proj4"
    self="$WORK/legacy-accept-self.txt"
    printf 'my-own-handle\nrepo=%s\n' "$base" > "$self"

    context_in "$root" "$self"
    [ "$CTX_RC" -eq 0 ] || { echo "  comm-context.sh exited $CTX_RC: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "stale" && { echo "  unexpected staleness notice for a matching legacy self-file: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "self-healed" || { echo "  missing self-heal notice: $CTX_ERR"; return 1; }
    [ "$CTX_NAME" = "my-own-handle" ] || { echo "  NAME=$CTX_NAME, want my-own-handle (accepted, not re-derived)"; return 1; }

    local lines; lines="$(wc -l < "$self")"
    [ "$lines" -ge 3 ] || { echo "  self-file not backfilled to v2 with root=: $(cat "$self")"; return 1; }
    local backfilled_root; backfilled_root="$(sed -n '3p' "$self" | sed -n 's/^root=//p')"
    [ "$backfilled_root" = "$root" ] || { echo "  backfilled root=$backfilled_root, want $root"; return 1; }

    # Second read: already v2 — no re-heal, no re-notify, same identity.
    context_in "$root" "$self"
    [ "$CTX_RC" -eq 0 ] || { echo "  second read exited $CTX_RC: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "self-healed" && { echo "  self-heal fired AGAIN on an already-v2 file: $CTX_ERR"; return 1; }
    [ "$CTX_NAME" = "my-own-handle" ] || { echo "  second read NAME=$CTX_NAME, want my-own-handle"; return 1; }
    return 0
}

case_legacy_self_file_mismatched_repo_still_discarded() {
    # (b) The pane-recycling protection root= replaced must still hold: a
    # legacy self-file whose repo= does NOT match is discarded as stale,
    # exactly as before this fix — self-healing must never launder a real
    # mismatch into acceptance.
    local root self
    mkdir -p "$WORK/legacy-mismatch/proj5"
    root="$(realpath "$WORK/legacy-mismatch/proj5")"
    self="$WORK/legacy-mismatch-self.txt"
    printf 'someone-elses-handle\nrepo=totally-different-repo\n' > "$self"

    context_in "$root" "$self"
    [ "$CTX_RC" -eq 0 ] || { echo "  exited $CTX_RC: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "stale" || { echo "  missing staleness notice: $CTX_ERR"; return 1; }
    [ -z "$CTX_NAME" ] || { echo "  NAME=$CTX_NAME, want empty (discarded)"; return 1; }
    local lines; lines="$(wc -l < "$self")"
    [ "$lines" -eq 2 ] || { echo "  mismatched legacy self-file was mutated (must be left untouched): $(cat "$self")"; return 1; }
    return 0
}

case_v2_self_file_wrong_root_is_still_discarded() {
    # (c) root= present-and-wrong stays strict and unconditional — this
    # fix only widens acceptance for files that PREDATE root=, never for
    # ones that carry it and disagree.
    local rootA rootB self
    mkdir -p "$WORK/v2-wrong-root/proj6a" "$WORK/v2-wrong-root/proj6b"
    rootA="$(realpath "$WORK/v2-wrong-root/proj6a")"
    rootB="$(realpath "$WORK/v2-wrong-root/proj6b")"
    self="$WORK/v2-wrong-root-self.txt"
    printf 'handle-for-a\nrepo=proj6a\nroot=%s\n' "$rootA" > "$self"

    context_in "$rootB" "$self"
    [ "$CTX_RC" -eq 0 ] || { echo "  exited $CTX_RC: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "stale" || { echo "  missing staleness notice: $CTX_ERR"; return 1; }
    [ -z "$CTX_NAME" ] || { echo "  NAME=$CTX_NAME, want empty (discarded — root= present and wrong)"; return 1; }
    local lines; lines="$(wc -l < "$self")"
    [ "$lines" -eq 3 ] || { echo "  self-file with a wrong root= was mutated (must be left untouched): $(cat "$self")"; return 1; }
    return 0
}

case_nopane_selffile_shared_across_repos_not_healed() {
    # Coordinator addendum, item 6: a shell with NO tmux pane collapses to
    # ONE self-file slot per host ("<host>__nopane.txt", see
    # comm-context.sh) shared by every such shell on this host. The
    # repo=/root= check is what makes that sharing safe — a background
    # shell in a DIFFERENT repo reading the same slot back must be
    # discarded, never healed into adopting the other repo's identity.
    local rootA rootB self
    mkdir -p "$WORK/nopane-cross-repo/repoA" "$WORK/nopane-cross-repo/repoB"
    rootA="$(realpath "$WORK/nopane-cross-repo/repoA")"
    rootB="$(realpath "$WORK/nopane-cross-repo/repoB")"
    self="$WORK/nopane-shared-self.txt"

    # repoA's session previously claimed the shared nopane slot (legacy,
    # pre-root=, so this ALSO exercises the self-heal boundary: it must
    # heal for repoA, never for repoB).
    printf 'repoA-handle\nrepo=repoA\n' > "$self"

    context_in "$rootB" "$self"
    [ "$CTX_RC" -eq 0 ] || { echo "  exited $CTX_RC: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "stale" || { echo "  missing staleness notice: $CTX_ERR"; return 1; }
    [ -z "$CTX_NAME" ] || { echo "  repoB read NAME=$CTX_NAME, want empty (must not adopt repoA's identity)"; return 1; }
    local lines; lines="$(wc -l < "$self")"
    [ "$lines" -eq 2 ] || { echo "  shared nopane self-file was mutated by the mismatched read: $(cat "$self")"; return 1; }

    # repoA reading its OWN slot back must still work, and now self-heals.
    context_in "$rootA" "$self"
    [ "$CTX_RC" -eq 0 ] || { echo "  repoA re-read exited $CTX_RC: $CTX_ERR"; return 1; }
    [ "$CTX_NAME" = "repoA-handle" ] || { echo "  repoA re-read NAME=$CTX_NAME, want repoA-handle"; return 1; }
    contains "$CTX_ERR" "self-healed" || { echo "  repoA re-read missing self-heal notice: $CTX_ERR"; return 1; }
    return 0
}

case_nopane_selffile_from_non_repo_cwd_not_healed_and_send_refuses() {
    # Tiny addendum (coordinator, field-corroborated): a background shell
    # cd'd OUTSIDE any repo entirely (REPO then reads as the cwd's own
    # basename, e.g. a scratchpad dir) trips the SAME repo-mismatch discard
    # from the other side — must not be healed either, and a send from
    # there must refuse loudly (item 5) rather than stamp a placeholder
    # sender. The fix for an operator here is "run from the repo", not
    # "rejoin".
    local self scratch
    mkdir -p "$WORK/nopane-scratch/some-scratchpad"
    scratch="$(realpath "$WORK/nopane-scratch/some-scratchpad")"
    self="$WORK/nopane-scratch-self.txt"
    printf 'repoA-handle\nrepo=repoA\n' > "$self"

    context_in "$scratch" "$self"
    [ "$CTX_RC" -eq 0 ] || { echo "  exited $CTX_RC: $CTX_ERR"; return 1; }
    contains "$CTX_ERR" "stale" || { echo "  missing staleness notice: $CTX_ERR"; return 1; }
    [ -z "$CTX_NAME" ] || { echo "  non-repo-cwd read NAME=$CTX_NAME, want empty (must not adopt repoA's identity)"; return 1; }
    local lines; lines="$(wc -l < "$self")"
    [ "$lines" -eq 2 ] || { echo "  self-file was mutated by a non-repo-cwd read: $(cat "$self")"; return 1; }

    # A send from here must refuse loudly (item 5), not stamp a
    # synthesized "unknown-<host>" sender.
    local send_out send_err send_rc errfile
    errfile="$WORK/send-scratch.err"
    send_out="$(cd "$scratch" && SOT_COMM_SELF_FILE="$self" SOT_COMM_TEST_HOST="$HOST" \
        "$SEND" @somebody "hello" 2>"$errfile")"
    send_rc=$?
    send_err="$(cat "$errfile" 2>/dev/null || true)"
    [ "$send_rc" -ne 0 ] || { echo "  comm-send.sh succeeded with no identity: $send_out"; return 1; }
    contains "$send_err" "unknown-" && { echo "  comm-send.sh still stamped a synthesized unknown-<host> sender: $send_err"; return 1; }
    contains "$send_err" "identity did not resolve" || { echo "  missing identity-refusal message: $send_err"; return 1; }
    return 0
}

case_legacy_unknown_root_row() {
    # Codex review F1 / simplicity audit: a registry row that predates this
    # feature (no `root` key at all) must count as a COLLISION for the
    # derivation algorithm, not a free pass — same fail-safe stance as the
    # self-file case above, at the registry layer instead.
    local root base parent h1 h2 legacy_obj
    mkdir -p "$WORK/legacytest/grpL/proj3"
    root="$(realpath "$WORK/legacytest/grpL/proj3")"
    base="proj3"; parent="grpL"
    h1="${base}-${HOST}"
    h2="${base}-${parent}-${HOST}"

    legacy_obj="$(jq -n --arg repo "$base" \
        '{host:"other",tmux:"",pane_id:"",repo:$repo,expertise:[],status:"idle",joined:"t",last_seen:"t"}')"
    jq --arg n "$h1" --argjson o "$legacy_obj" '.agents[$n] = $o' "$REGISTRY" > "$REGISTRY.tmp" \
        && mv "$REGISTRY.tmp" "$REGISTRY"

    join_in "$root"
    [ "$JOIN_RC" -eq 0 ] || { echo "  exited $JOIN_RC: $JOIN_ERR"; return 1; }
    contains "$JOIN_OUT" "Joined sot-comm as @$h2" \
        || { echo "  stdout: $JOIN_OUT (want escalation to @$h2 — an unknown root must not be a free pass)"; return 1; }
    [ "$(registry_root "$h2")" = "$root" ] || { echo "  h2 root=$(registry_root "$h2"), want $root"; return 1; }
    [ "$(registry_has_root_key "$h1")" = "no" ] \
        || { echo "  legacy row for @$h1 was mutated (now has a root key): $(registry_root "$h1")"; return 1; }
    return 0
}

case_join_warns_on_stranding_escalation_when_bridge_running() {
    # Coordinator ruling item 2: comm-join.sh must warn LOUDLY — never
    # silently strand — when a derived join is about to escalate AWAY from
    # the bare tier-1 handle AND a listener bridge for that bare handle is
    # already running under this uid: the near-certain signature of this
    # session's OWN evicted identity (case_legacy_unknown_root_row above is
    # the exact registry shape that forces this escalation), not a real
    # collision with an unrelated project.
    local root base parent h1 h2 legacy_obj
    mkdir -p "$WORK/strandtest/grpZ/proj7"
    root="$(realpath "$WORK/strandtest/grpZ/proj7")"
    base="proj7"; parent="grpZ"
    h1="${base}-${HOST}"
    h2="${base}-${parent}-${HOST}"

    legacy_obj="$(jq -n --arg repo "$base" \
        '{host:"other",tmux:"",pane_id:"",repo:$repo,expertise:[],status:"idle",joined:"t",last_seen:"t"}')"
    jq --arg n "$h1" --argjson o "$legacy_obj" '.agents[$n] = $o' "$REGISTRY" > "$REGISTRY.tmp" \
        && mv "$REGISTRY.tmp" "$REGISTRY"

    if ! command -v tmux >/dev/null 2>&1; then
        echo "  SKIP: no tmux on this host — cannot mock a bridge marker (reporting as pass; see report for this deviation)"
        return 0
    fi
    # Mock the bridge marker: comm-listen.sh's own naming, a detached tmux
    # session "commbridge-<handle>" on this suite's ISOLATED $SOT_TMUX_SOCK
    # (the same seam case_spawn_fresh_only_refusal already uses) — never
    # the real per-user socket. `sleep` stands in for the reconnect loop;
    # comm-join.sh's guard only checks that the session exists.
    if ! tmux -S "$SOT_TMUX_SOCK" new-session -d -s "commbridge-$h1" "sleep 60" 2>/dev/null; then
        echo "  SKIP: could not create a mock bridge tmux session on the isolated socket — unmockable in this environment (reporting as pass; see report for this deviation)"
        return 0
    fi

    join_in "$root"
    tmux -S "$SOT_TMUX_SOCK" kill-session -t "commbridge-$h1" 2>/dev/null || true

    [ "$JOIN_RC" -eq 0 ] || { echo "  comm-join.sh exited $JOIN_RC: $JOIN_ERR"; return 1; }
    contains "$JOIN_OUT" "Joined sot-comm as @$h2" \
        || { echo "  stdout: $JOIN_OUT (want escalation to @$h2, same as the no-bridge case)"; return 1; }
    contains "$JOIN_ERR" "WARNING" || { echo "  missing the stranding warning: $JOIN_ERR"; return 1; }
    contains "$JOIN_ERR" "$h1" || { echo "  warning doesn't name the bare handle @$h1: $JOIN_ERR"; return 1; }
    contains "$JOIN_ERR" "comm-leave.sh --name $h2" \
        || { echo "  warning missing the exact reclaim recipe (comm-leave.sh --name $h2): $JOIN_ERR"; return 1; }
    contains "$JOIN_ERR" "comm-join.sh --name $h1" \
        || { echo "  warning missing the exact reclaim recipe (comm-join.sh --name $h1): $JOIN_ERR"; return 1; }
    return 0
}

case_spawn_fresh_only_refusal() {
    # Codex review F3: comm-spawn.sh must NEVER reclaim an existing row —
    # even one sharing its own project root — the way comm-join.sh does.
    # Set up a LIVE-looking row via an ordinary join (status "idle", as a
    # real join sets), then spawn against the same root with no --name:
    # the live row must survive untouched, and the new agent must land on
    # a DIFFERENT (escalated) handle instead of clobbering it.
    local root base parent h1 h2
    mkdir -p "$WORK/spawntest/grpS/proj"
    root="$(realpath "$WORK/spawntest/grpS/proj")"
    base="proj"; parent="grpS"
    h1="${base}-${HOST}"
    h2="${base}-${parent}-${HOST}"

    join_in "$root"
    [ "$JOIN_RC" -eq 0 ] || { echo "  setup join exited $JOIN_RC: $JOIN_ERR"; return 1; }
    contains "$JOIN_OUT" "Joined sot-comm as @$h1" || { echo "  setup join stdout: $JOIN_OUT"; return 1; }

    spawn_in "$root"
    [ "$SPAWN_RC" -eq 0 ] || { echo "  comm-spawn.sh exited $SPAWN_RC: $SPAWN_ERR"; return 1; }
    contains "$SPAWN_OUT" "Spawned (raw) @$h2" \
        || { echo "  spawn stdout: $SPAWN_OUT (want escalation to @$h2, not a reclaim of @$h1)"; return 1; }

    [ "$(registry_field "$h1" status)" = "idle" ] \
        || { echo "  @$h1 (the live row) was overwritten by spawn: status=$(registry_field "$h1" status)"; return 1; }
    [ "$(registry_root "$h1")" = "$root" ] \
        || { echo "  @$h1 root changed by spawn: $(registry_root "$h1")"; return 1; }

    [ "$(registry_root "$h2")" = "$root" ] || { echo "  @$h2 root=$(registry_root "$h2"), want $root"; return 1; }
    [ "$(registry_field "$h2" status)" = "spawning" ] \
        || { echo "  @$h2 status=$(registry_field "$h2" status), want spawning"; return 1; }
    tmux -S "$SOT_TMUX_SOCK" has-session -t "$h2" 2>/dev/null \
        || { echo "  no isolated tmux session for @$h2"; return 1; }
    return 0
}

case_lock_closes_derive_write_gap() {
    # Deterministic simulation of the race claim_derived_handle exists to
    # close: two derived joins for DIFFERENT roots that would both decide on
    # the bare tier-1 handle if "derive" and "registry_put" were not one
    # locked step. True concurrency isn't reproducible deterministically in
    # bash, so this uses the registry lock itself as the synchronization
    # point instead of timing:
    #   1. We seize $LOCKDIR ourselves (standing in for "another process
    #      already holds the claim critical section").
    #   2. We start a second derived join (root B) in the BACKGROUND, with
    #      $SOT_COMM_TEST_LOCK_BARRIER pointed at a file with_lock touches
    #      right before its first mkdir attempt (Codex review F10 — this
    #      is the REAL handshake; a sleep, no matter how generous, could
    #      only ever make the failure mode LESS likely to reproduce, never
    #      prove the fix).
    #   3. We wait for that barrier file — bounded, so a child that never
    #      reaches its lock attempt fails the test instead of hanging it —
    #      then mutate the registry as if a THIRD, already-locked claim
    #      (root A) just landed, and release the lock.
    #   4. The backgrounded join can only ever observe the registry AFTER
    #      that mutation once it finally acquires the lock. If derive+put
    #      were not atomic (the pre-fix shape: decide the name, unlocked,
    #      then lock only to write), it would have "decided" tier 1 before
    #      ever touching the lock and clobbered root A's row regardless of
    #      what happened while it waited. With the fix, it must re-derive
    #      under the lock and see the collision.
    local rootA rootB rbase="racer" rh1 rh2 mutate_obj self out errfile pid rc barrier deadline
    mkdir -p "$WORK/lockrace-a/grp/racer"
    mkdir -p "$WORK/lockrace-b/grp/racer"
    rootA="$(realpath "$WORK/lockrace-a/grp/racer")"
    rootB="$(realpath "$WORK/lockrace-b/grp/racer")"
    rh1="${rbase}-${HOST}"
    rh2="${rbase}-grp-${HOST}"

    mkdir "$LOCKDIR" || { echo "  could not seize the test lock (already held?)"; return 1; }

    next_self_file; self="$NEXT_SELF_FILE"
    out="$WORK/lockrace.out"; errfile="$WORK/lockrace.err"
    barrier="$WORK/lockrace.barrier"
    rm -f "$barrier"
    ( cd "$rootB" && SOT_COMM_SELF_FILE="$self" SOT_COMM_NAME="" \
        SOT_COMM_TEST_LOCK_BARRIER="$barrier" "$JOIN" >"$out" 2>"$errfile" ) &
    pid=$!

    deadline=$(( $(date +%s) + 10 ))
    until [ -e "$barrier" ]; do
        if [ "$(date +%s)" -ge "$deadline" ]; then
            echo "  timed out waiting for the backgrounded join to reach its lock attempt"
            kill -9 "$pid" 2>/dev/null || true
            wait "$pid" 2>/dev/null || true
            rmdir "$LOCKDIR" 2>/dev/null || true
            return 1
        fi
        sleep 0.02
    done

    mutate_obj="$(jq -n --arg root "$rootA" \
        '{host:"other",tmux:"",pane_id:"",repo:"racer",root:$root,expertise:[],status:"idle",joined:"t",last_seen:"t"}')"
    jq --arg n "$rh1" --argjson o "$mutate_obj" '.agents[$n] = $o' "$REGISTRY" > "$REGISTRY.tmp" \
        && mv "$REGISTRY.tmp" "$REGISTRY"

    rmdir "$LOCKDIR"

    # Bounded wait on the child too (Codex review F10): a live-stuck child
    # must fail the test, not hang it forever.
    deadline=$(( $(date +%s) + 10 ))
    while kill -0 "$pid" 2>/dev/null; do
        if [ "$(date +%s)" -ge "$deadline" ]; then
            echo "  backgrounded join did not finish within 10s after the lock was released"
            kill -9 "$pid" 2>/dev/null || true
            wait "$pid" 2>/dev/null || true
            return 1
        fi
        sleep 0.05
    done
    wait "$pid"; rc=$?

    local bg_out bg_err
    bg_out="$(cat "$out" 2>/dev/null || true)"
    bg_err="$(cat "$errfile" 2>/dev/null || true)"

    [ "$rc" -eq 0 ] || { echo "  backgrounded comm-join.sh exited $rc: $bg_err"; return 1; }
    contains "$bg_out" "Joined sot-comm as @$rh2" \
        || { echo "  stdout: $bg_out (want @$rh2 — a race window let it clobber @$rh1 instead)"; return 1; }
    [ "$(registry_root "$rh1")" = "$rootA" ] \
        || { echo "  @$rh1 (the interleaved claim) was clobbered: root=$(registry_root "$rh1")"; return 1; }
    [ "$(registry_root "$rh2")" = "$rootB" ] \
        || { echo "  @$rh2 root=$(registry_root "$rh2"), want $rootB"; return 1; }
    return 0
}

case_rollback_survives_replacement_row() {
    # Codex review PR #148 round 2, finding 1: registry_del_if_provisional
    # must NOT delete a row that has since been replaced — reproduced by
    # the round-2 reviewer as an unconditional `registry_del "$NAME"`
    # deleting a genuinely live row a child had already written. Drives
    # the SAME function comm-spawn.sh's rollback trap calls (comm-lib.sh),
    # not a reimplementation, by sourcing comm-lib.sh directly (see the
    # top of this file).
    local name="rollback-test-agent" root="/fake/root/for/rollback-test"
    local nonce="nonce-abc-123" prov replacement

    prov="$(jq -n --arg root "$root" --arg nonce "$nonce" \
        '{host:"h",tmux:"",pane_id:"",repo:"r",root:$root,expertise:[],status:"spawning",joined:"t0",last_seen:"t0",nonce:$nonce}')"
    with_lock registry_put "$name" "$prov"

    # Simulate "the child joined for real" (or an explicit claimant took
    # over) BEFORE the spawner's rollback runs: a live row, no nonce.
    replacement="$(jq -n --arg root "$root" \
        '{host:"h",tmux:"session:1.1",pane_id:"%1",repo:"r",root:$root,expertise:[],status:"idle",joined:"t1",last_seen:"t1"}')"
    with_lock registry_put "$name" "$replacement"

    with_lock registry_del_if_provisional "$name" "$root" "$nonce"
    local rc=$?
    [ "$rc" -eq 2 ] || { echo "  registry_del_if_provisional returned $rc, want 2 (not-ours-anymore)"; return 1; }

    [ "$(registry_field "$name" status)" = "idle" ] \
        || { echo "  the replacement row was deleted/altered: status=$(registry_field "$name" status)"; return 1; }
    [ "$(registry_field "$name" tmux)" = "session:1.1" ] \
        || { echo "  the replacement row's tmux field is gone: $(registry_field "$name" tmux)"; return 1; }

    # Sanity check the OTHER branch too: an UNREPLACED provisional row
    # (matching root+nonce+status) must still be deletable.
    local name2="rollback-test-agent-2"
    with_lock registry_put "$name2" "$prov"
    with_lock registry_del_if_provisional "$name2" "$root" "$nonce"
    rc=$?
    [ "$rc" -eq 0 ] || { echo "  an untouched provisional row was NOT deleted: rc=$rc"; return 1; }
    [ "$(registry_field "$name2" status)" = "MISSING" ] \
        || { echo "  provisional row for @$name2 still present after a claimed rollback"; return 1; }

    with_lock registry_del "$name" >/dev/null 2>&1 || true
    return 0
}

case_with_lock_restores_prior_trap_on_failure() {
    # Codex review PR #148 round 2, finding 4: a callee that fails
    # DIRECTLY (not via `|| true`) under a caller's `set -e` must still
    # leave the caller's own prior EXIT trap intact — it used to be lost
    # (only with_lock's own lock-release trap fired) because `"$@"` ran as
    # a bare statement that aborted the whole subprocess right there,
    # skipping the restore lines below it. Needs a REAL subprocess with
    # its own `set -e` and its own prior trap — this test script itself
    # doesn't run under `set -e`, so the bug can't reproduce inline.
    local marker="$WORK/trap-marker.txt" script="$WORK/trap-restore-check.sh"
    rm -f "$marker"
    cat > "$script" <<SCRIPT
#!/usr/bin/env bash
set -euo pipefail
source "$SCRIPTS_DIR/comm-lib.sh"
ensure_home
trap 'echo prior-trap-fired > "$marker"' EXIT
fail_cmd() { return 7; }
with_lock fail_cmd
echo UNREACHABLE >&2
SCRIPT
    bash "$script" >/dev/null 2>"$WORK/trap-restore.err"
    local rc=$?
    [ "$rc" -ne 0 ] || { echo "  subprocess did not fail as expected (rc=0): $(cat "$WORK/trap-restore.err")"; return 1; }
    [ -f "$marker" ] || { echo "  prior EXIT trap did not fire (no marker file); stderr: $(cat "$WORK/trap-restore.err")"; return 1; }
    contains "$(cat "$marker")" "prior-trap-fired" || { echo "  marker content wrong: $(cat "$marker")"; return 1; }
    [ ! -d "$LOCKDIR" ] || { echo "  lock dir leaked after the failure"; return 1; }
    return 0
}

case_hash_command_failure_fails_loudly() {
    # Codex review PR #148 round 2, finding 5: an installed-but-FAILING
    # sha256sum must not be silently accepted as success with an empty
    # hash. Both sha256sum AND shasum are faked to exit nonzero (sot_hash6
    # tries shasum as a fallback when sha256sum fails — faking only
    # sha256sum would just exercise that fallback path onto the REAL
    # shasum and succeed, not test the failure path at all) and put ahead
    # of the real ones on PATH for one comm-join.sh call, forced to reach
    # tier 3 (tiers 1-2 already taken, by ROOT1/ROOT2 from earlier cases)
    # where a hash is actually needed.
    local fakebin="$WORK/fakebin"
    mkdir -p "$fakebin"
    cat > "$fakebin/sha256sum" <<'FAKESHA'
#!/bin/sh
exit 23
FAKESHA
    cp "$fakebin/sha256sum" "$fakebin/shasum"
    chmod +x "$fakebin/sha256sum" "$fakebin/shasum"

    mkdir -p "$WORK/site4/groupX/instructor-materials"
    local root; root="$(realpath "$WORK/site4/groupX/instructor-materials")"

    JOIN_PATH_PREFIX="$fakebin"
    join_in "$root"
    JOIN_PATH_PREFIX=""

    [ "$JOIN_RC" -ne 0 ] || { echo "  expected a nonzero exit with a broken sha256sum, got 0: $JOIN_OUT"; return 1; }
    contains "$JOIN_ERR" "sot_hash6" || { echo "  missing sot_hash6 failure reason: $JOIN_ERR"; return 1; }
    contains "$JOIN_OUT" "Joined sot-comm as @" \
        && { echo "  claimed a handle despite the broken hash tool: $JOIN_OUT"; return 1; }
    return 0
}

case_host_alias_guard_triggers_on_long_host() {
    # CI incident follow-up (round 3): every OTHER case pins HOST short and
    # clean specifically so the F7 host-alias guard never fires — which
    # means the guard itself would otherwise have ZERO positive coverage in
    # this suite. Deliberately route a long host through the SAME
    # $SOT_COMM_TEST_HOST seam for just this one call, and confirm the
    # digest-suffix transformation actually happens.
    #
    # The expected value here necessarily mirrors sot_sanitize_component's
    # clamp + sot_hash6's algorithm — that's not "a parallel implementation
    # that can drift" in the sense the fix direction warned against (that
    # warning was about NOT computing per-host expectations for the OTHER,
    # host-agnostic cases — the fix there is pinning the input, not
    # replicating the transform). Here the transform IS the thing under
    # test, so asserting its exact output requires computing what it
    # should produce — using the SAME sha256sum tool, not a hand-rolled
    # hash.
    local root long_host sanitized_prefix hash6 expected_handle saved_host
    mkdir -p "$WORK/hosttest/proj-host"
    root="$(realpath "$WORK/hosttest/proj-host")"
    long_host="ci-runner-with-a-long-dirty-hostname-example"

    sanitized_prefix="${long_host:0:12}"
    hash6="$(printf '%s' "$long_host" | sha256sum | cut -c1-6)"
    expected_handle="proj-host-${sanitized_prefix}-${hash6}"

    saved_host="$SOT_COMM_TEST_HOST"
    export SOT_COMM_TEST_HOST="$long_host"
    join_in "$root"
    export SOT_COMM_TEST_HOST="$saved_host"

    [ "$JOIN_RC" -eq 0 ] || { echo "  exited $JOIN_RC: $JOIN_ERR"; return 1; }
    contains "$JOIN_OUT" "Joined sot-comm as @$expected_handle" \
        || { echo "  stdout: $JOIN_OUT (want @$expected_handle — sanitized-prefix + '-' + digest-of-raw-host)"; return 1; }
    [ "$(registry_root "$expected_handle")" = "$root" ] \
        || { echo "  root=$(registry_root "$expected_handle"), want $root"; return 1; }
    return 0
}

# --- run, in order (later cases depend on earlier ones' registry state) --

check "fresh claim records root"                            case_fresh_claim
check "same-root rejoin keeps bare handle"                  case_same_root_rejoin
check "different-root collision -> parentdir-qualified, first entry intact" case_diff_root_collision
check "three-way collision -> hash-qualified handle"         case_three_way_collision
check "explicit --name is verbatim even when it collides"    case_explicit_name_verbatim
check "SOT_COMM_NAME env is verbatim even when it collides"  case_env_name_verbatim
check "legacy self-file (repo= matches, no root=) is reclaimed via comm-join.sh, not re-derived" case_legacy_matching_self_file_is_reclaimed_not_rederived
check "(a) legacy self-file, matching repo: comm-context.sh accepts + backfills root=" case_legacy_self_file_matching_repo_accepted_and_backfilled
check "(b) legacy self-file, mismatched repo: still discarded as stale"     case_legacy_self_file_mismatched_repo_still_discarded
check "(c) v2 self-file, root= present but wrong: still discarded as stale" case_v2_self_file_wrong_root_is_still_discarded
check "nopane self-file shared across repos: mismatched read discarded, never healed" case_nopane_selffile_shared_across_repos_not_healed
check "nopane self-file read from a non-repo cwd: discarded, not healed; a send from there refuses loudly" case_nopane_selffile_from_non_repo_cwd_not_healed_and_send_refuses
check "legacy registry row with no root= is a collision, not a free pass" case_legacy_unknown_root_row
check "comm-join.sh warns loudly on stranding escalation when a bridge for the bare handle is running" case_join_warns_on_stranding_escalation_when_bridge_running
check "comm-spawn.sh fresh-mode refuses to reclaim a live row (F3)" case_spawn_fresh_only_refusal
check "concurrent claim landing mid-wait is not clobbered (lock closes the derive/write gap)" case_lock_closes_derive_write_gap
check "rollback never deletes a row that replaced the provisional one (F1 round 2)" case_rollback_survives_replacement_row
check "with_lock restores the caller's prior EXIT trap after a direct callee failure (F2 round 2)" case_with_lock_restores_prior_trap_on_failure
check "a failing hash command fails loudly instead of an empty-hash handle (F5 round 2)" case_hash_command_failure_fails_loudly
check "a long/dirty host triggers the F7 host-alias digest suffix" case_host_alias_guard_triggers_on_long_host

echo ""
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
