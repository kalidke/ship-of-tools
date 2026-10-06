#!/usr/bin/env bash
# test-join-disambiguation.sh — self-contained test for the derived-handle
# disambiguation feature (ADR 0028 addendum: "derived vs explicit"). No bats
# dependency. HERMETIC: runs against a temp $SOT_COMM_HOME, a temp self-file
# per simulated session (via $SOT_COMM_SELF_FILE), a PINNED host (via
# $SOT_COMM_TEST_HOST — see below), and a dead daemon endpoint (via
# $SOT_TMUX_SOCK) for the comm-spawn.sh case — never touches the real
# ~/.sot-comm or a real daemon (a comm-spawn.sh smoke run during this
# feature's development that omitted the endpoint isolation created
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
# Usage: comm/tests/test-join-disambiguation.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

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
guard_fresh_home "$WORK"; guard_refuse_live_home "$SOT_COMM_HOME"
SCRIPTS_DIR="$(guard_stage_bin "$WORK")" || exit 2
# Re-point SCRIPT_DIR at the real scripts dir (it starts out as THIS test
# file's own dir, comm/tests): LU6e's pipe: cases below call
# sot_oneshot_request directly, with SCRIPT_DIR set as every real caller
# (sot-fe, comm-relay.sh) sets it. Not read again after this point for
# anything else in this file.
SCRIPT_DIR="$SCRIPTS_DIR"
JOIN="$SCRIPTS_DIR/comm-join.sh"
SPAWN="$SCRIPTS_DIR/comm-spawn.sh"
CONTEXT="$SCRIPTS_DIR/comm-context.sh"
SEND="$SCRIPTS_DIR/comm-send.sh"
# 0031 B1: the record a daemon writes at startup. Without it no script
# appends locally, and every filing here would go to a daemon instead.
mkdir -p "$SOT_COMM_HOME/inbox"
# A fake findmnt first on PATH reports a local filesystem, so the record and
# every script's identity are `local <machine-id>` whatever $WORK sits on (a
# function stub would not survive: comm-lib.sh defines _sot_findmnt itself).
mkdir -p "$WORK/findmnt-bin"
printf '#!/bin/sh\necho "ext4 rw,relatime /dev/fake"\n' > "$WORK/findmnt-bin/findmnt"
chmod +x "$WORK/findmnt-bin/findmnt"
export PATH="$WORK/findmnt-bin:$PATH"
bash -c 'source "$1"; sot_inbox_lock_identity "$INBOX_DIR"' _ "$SCRIPTS_DIR/comm-lib.sh" > "$SOT_COMM_HOME/inbox-lock-manager"
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
# The registry lock's path (comm-lib.sh's _SOT_REG_LOCK). A directory there is
# an older peer's lock, which the file lock never reclaims — used by
# case_lock_closes_derive_write_gap below to simulate a concurrent claim
# landing WHILE a derived join is blocked waiting for the lock.
LOCKDIR="$SOT_COMM_HOME/.registry.lock"
# comm-spawn.sh always goes through the daemon: pin a dead endpoint so no
# case in this suite can ever reach a real sotd (a case that needs a daemon
# starts the stub below and points at it explicitly). A dummy token keeps
# the hello frame off the real token file.
export SOT_SPAWN_ENDPOINT="unix:$WORK/no-daemon.sock"
export SOT_TOKEN="dummy-test-token"
unset SOT_SOCKET SOT_WORKSPACE_ID
trap 'stop_stub_daemon; rm -rf "${WORK:?}"' EXIT

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
SKIP=0

# check DESC FN — run FN (which prints diagnostics and returns 0 = pass,
# 2 = SKIP, anything else = fail), then print the required PASS/FAIL/SKIP
# line. SKIP is a DISTINCT outcome, never folded into PASS (Codex review
# round-1 finding 5): a case that can't exercise its guard in this
# environment (an unmockable resource) used to just `return 0`
# after printing its own inline "SKIP:" diagnostic, which this function
# then reported as a bare PASS — an unexecuted guard counted as verified.
# A case that cannot run its check must say so in the tally, not just in
# an easy-to-miss diagnostic line.
check() {
    local desc="$1" fn="$2"
    local rc
    "$fn"
    rc=$?
    case "$rc" in
        0) echo "PASS: $desc"; PASS=$((PASS + 1)) ;;
        2) echo "SKIP: $desc"; SKIP=$((SKIP + 1)) ;;
        *) echo "FAIL: $desc"; FAIL=$((FAIL + 1)) ;;
    esac
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

# spawn_in ROOT [ARGS...] — run comm-spawn.sh against ROOT (the daemon is
# whatever SOT_SPAWN_ENDPOINT names: the dead default, or a case's stub).
# Sets SPAWN_OUT / SPAWN_ERR / SPAWN_RC.
SPAWN_OUT=""; SPAWN_ERR=""; SPAWN_RC=0
spawn_in() {
    local root="$1"; shift
    local errfile="$WORK/spawn-stderr.tmp"
    SPAWN_OUT="$("$SPAWN" "$root" "$@" 2>"$errfile")"
    SPAWN_RC=$?
    SPAWN_ERR="$(cat "$errfile" 2>/dev/null || true)"
}

# --- stub daemon (nc -klU + FIFO, the test-spawn-capsule-workspace.sh
# harness) --- answers hello, workspace.create (WSID/SLUG), workspace.list
# (that one row, phase ready) and pty.input (ok, enter sent); logs every
# request to STUB_REQLOG. Started per case, stopped by the case; a real
# sotd is never reached.
STUB_SOCK=""; STUB_REQLOG=""; STUB_NC_PID=""; STUB_WATCHER_PID=""; STUBN=0
start_stub_daemon() {  # WSID SLUG ROOT [HANDLE]
    # HANDLE is what the row DECLARES through agent.join, which is how the
    # daemon publishes a row's sot-comm handle. A stub that leaves it empty
    # models a row that never joined.
    local wsid="$1" slug="$2" root="$3" handle="${4:-}" fifo hello create list ptyin
    STUBN=$((STUBN + 1))
    STUB_SOCK="$WORK/stub-$STUBN.sock"; fifo="$WORK/stub-$STUBN.fifo"; STUB_REQLOG="$WORK/stub-$STUBN.log"
    mkfifo "$fifo"; : > "$STUB_REQLOG"
    hello='{"v":1,"id":1,"kind":"res","op":"hello","payload":{"session_id":"s1","revision":0,"snapshot_pending":false}}'
    create="$(jq -nc --arg id "$wsid" --arg slug "$slug" --arg root "$root" \
        '{v:1,id:1,kind:"res",op:"workspace.create",payload:{workspace_id:$id,slug:$slug,label:$slug,project_root:$root}}')"
    list="$(jq -nc --arg id "$wsid" --arg slug "$slug" --arg root "$root" --arg h "$handle" \
        '{v:1,id:1,kind:"res",op:"workspace.list",payload:{workspaces:[{workspace_id:$id,slug:$slug,label:$slug,project_root:$root,kernel_running:false,is_default:false,autostart_claude:true,agent:"claude",agent_name:"",agent_handle:$h,task:"",agent_state:"",agent_summary:"",agent_status_at:"",repl_state:"idle",runtime:"capsule",phase:"ready"}]}}')"
    ptyin='{"v":1,"id":1,"kind":"res","op":"pty.input","payload":{"ok":true,"bytes":1,"runtime":"capsule","enter":"sent"}}'
    exec 3<>"$fifo"
    nc -klU "$STUB_SOCK" < "$fifo" >> "$STUB_REQLOG" &
    STUB_NC_PID=$!
    ( created=0
      tail -n +1 -F "$STUB_REQLOG" 2>/dev/null | while IFS= read -r line; do
        case "$(printf '%s' "$line" | jq -r '.op // empty' 2>/dev/null)" in
            hello)            printf '%s\n' "$hello" >&3 ;;
            workspace.create) created=1; printf '%s\n' "$create" >&3 ;;
            workspace.list)
                if [ "$created" -eq 0 ] && [ -n "${PRE_CREATE_LIST:-}" ]; then
                    printf '%s\n' "{\"v\":1,\"id\":1,\"kind\":\"res\",\"op\":\"workspace.list\",\"payload\":{\"workspaces\":${PRE_CREATE_LIST}}}" >&3
                else
                    printf '%s\n' "$list" >&3
                fi ;;
            pty.input)        printf '%s\n' "$ptyin" >&3 ;;
        esac
      done ) &
    STUB_WATCHER_PID=$!
    local deadline=$((SECONDS + 5))
    while [ ! -S "$STUB_SOCK" ]; do [ "$SECONDS" -lt "$deadline" ] || break; sleep 0.05; done
}
stop_stub_daemon() {
    [ -n "$STUB_WATCHER_PID" ] && pkill -TERM -P "$STUB_WATCHER_PID" >/dev/null 2>&1
    [ -n "$STUB_WATCHER_PID" ] && kill "$STUB_WATCHER_PID" >/dev/null 2>&1
    [ -n "$STUB_NC_PID" ] && kill "$STUB_NC_PID" >/dev/null 2>&1
    [ -n "$STUB_WATCHER_PID" ] && wait "$STUB_WATCHER_PID" 2>/dev/null
    [ -n "$STUB_NC_PID" ] && wait "$STUB_NC_PID" 2>/dev/null
    exec 3>&- 2>/dev/null || true
    STUB_NC_PID=""; STUB_WATCHER_PID=""
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

# ADR 0046 decision 1: `sot_oneshot_request` (comm-lib.sh) now calls the
# ONE shared `sot_hello_frame` directly — this test used to carry its own
# `_sot_hello` copy purely to satisfy that function's old "the caller's
# scope defines it" contract, which no longer exists.

# --- cases -----------------------------------------------------------

. "$(dirname "${BASH_SOURCE[0]}")/join_disambiguation/derive.sh"
. "$(dirname "${BASH_SOURCE[0]}")/join_disambiguation/self_file.sh"
. "$(dirname "${BASH_SOURCE[0]}")/join_disambiguation/send_identity.sh"
. "$(dirname "${BASH_SOURCE[0]}")/join_disambiguation/spawn_and_lock.sh"
. "$(dirname "${BASH_SOURCE[0]}")/join_disambiguation/jq_args.sh"
. "$(dirname "${BASH_SOURCE[0]}")/join_disambiguation/pipe_endpoint.sh"
. "$(dirname "${BASH_SOURCE[0]}")/join_disambiguation/slot_guard.sh"

# --- run, in order (later cases depend on earlier ones' registry state) --

check "fresh claim records root"                            case_fresh_claim
check "same-root rejoin keeps bare handle"                  case_same_root_rejoin
check "different-root collision -> parentdir-qualified, first entry intact" case_diff_root_collision
check "three-way collision -> hash-qualified handle"         case_three_way_collision
check "explicit --name is verbatim even when it collides"    case_explicit_name_verbatim
check "SOT_COMM_NAME env is verbatim even when it collides"  case_env_name_verbatim
check "claim_derived_handle tier-1: CLAIMED_NAME/QUALIFIER/TIER1 parse correctly (round-1 F1)" case_claim_derived_handle_tier1_three_field_parse
check "legacy self-file (repo= matches, no root=) is reclaimed via comm-join.sh, not re-derived (registry-corroborated)" case_legacy_matching_self_file_is_reclaimed_not_rederived
check "(a) legacy self-file, matching repo: comm-context.sh accepts + backfills root=" case_legacy_self_file_matching_repo_accepted_and_backfilled
check "(b) legacy self-file, mismatched repo: still discarded as stale"     case_legacy_self_file_mismatched_repo_still_discarded
check "(c) v2 self-file, root= present but wrong: still discarded as stale" case_v2_self_file_wrong_root_is_still_discarded
check "(d) v2 self-file, root= present but empty: discarded, not treated as absent (round-1 F2)" case_v2_self_file_empty_root_is_discarded_not_treated_as_absent
check "malformed third line (not root=...) discarded, not treated as absent (round-3 F2)" case_malformed_third_line_discarded_not_treated_as_absent
check "unreadable/malformed registry refuses to heal or write (round-3 F1)" case_registry_read_error_refuses_to_heal_or_write
check "legacy self-file + a DISAGREEING registry root: refuses to self-heal (round-1 F2 ship-blocker)" case_legacy_selffile_registry_root_disagreement_refuses_heal
check "legacy self-file + an unknown-root registry row: still heals on repo match" case_legacy_selffile_unknown_root_registry_row_still_heals_on_repo_match
check "ancient one-line self-file WITH a matching-root registry row: heals" case_ancient_oneline_with_matching_registry_heals
check "ancient one-line self-file WITHOUT registry corroboration: discarded" case_ancient_oneline_without_registry_match_discarded
check "self-heal write failure is reported loudly, file left intact (round-1 F3)" case_self_heal_write_failure_reported_loudly_file_intact
check "comm-context.sh host part: pinned, raw hostname -s with case kept, plain hostname fallback" case_context_host_part_follows_the_raw_host_rule
check "nopane self-file shared across repos: mismatched read discarded, never healed" case_nopane_selffile_shared_across_repos_not_healed
check "nopane + same-basename DIFFERENT root: basename alone must not heal (round-2 F-A)" case_nopane_same_basename_different_root_discarded
check "nopane + same-basename NON-repo cwd: basename alone must not heal (round-2 F-A)" case_nopane_same_basename_non_repo_cwd_discarded
check "nopane WITH a matching registry root: heals (round-2 F-A positive path)" case_nopane_with_matching_registry_root_heals
check "nopane self-file read from a non-repo cwd: discarded, not healed; a send from there refuses loudly" case_nopane_selffile_from_non_repo_cwd_not_healed_and_send_refuses
check "comm-relay.sh send refuses with no resolved identity" case_comm_relay_send_refuses_with_no_identity
check "comm-bootstrap.sh refuses with no resolved identity" case_comm_bootstrap_refuses_with_no_identity
check "comm-send.sh files and types nothing, with a daemon or without one" case_send_files_and_types_nothing
check "comm-relay.sh send fails loudly with no reachable daemon, never claims 'relayed' (round-3 F3)" case_relay_send_fails_loudly_with_no_reachable_daemon
check "comm-send.sh succeeds with two genuinely rooted, registered identities (round-3 F8 positive path)" case_send_succeeds_with_rooted_registry_row
check "comm-send.sh refuses when NAME resolves but has no registry row (round-2 F4/C)" case_send_refuses_when_registry_row_missing_despite_resolved_name
check "comm-send.sh refuses when the registry row belongs to a different project (round-2 F4/C)" case_send_refuses_when_registry_root_mismatches_current_project
check "legacy registry row with no root= is a collision, not a free pass" case_legacy_unknown_root_row
check "comm-join.sh warns loudly on stranding escalation when the bare handle has a fresh heartbeat, and not on a stale or absent one" case_join_warns_on_stranding_escalation_when_the_bare_handle_is_live
check "comm-spawn.sh fresh-mode refuses to reclaim a live row (F3)" case_spawn_fresh_only_refusal
check "comm-spawn.sh --task refuses with no spawner identity, no-task spawn still works (round-2 SHOULD-FIX 3/G)" case_spawn_refuses_task_when_spawner_has_no_identity
check "comm-spawn.sh --task refuses when the spawner's registry row is gone (round-3 F4)" case_spawn_task_refuses_when_spawner_has_no_registry_row
check "concurrent claim landing mid-wait is not clobbered (lock closes the derive/write gap)" case_lock_closes_derive_write_gap
check "rollback never deletes a row that replaced the provisional one (F1 round 2)" case_rollback_survives_replacement_row
check "with_lock restores the caller's prior EXIT trap after a direct callee failure (F2 round 2)" case_with_lock_restores_prior_trap_on_failure
check "a failing hash command fails loudly instead of an empty-hash handle (F5 round 2)" case_hash_command_failure_fails_loudly
check "a long/dirty host triggers the F7 host-alias digest suffix" case_host_alias_guard_triggers_on_long_host
check "a pinned SOT_COMM_NAME never adopts a name from any self-file (capsule-comm-identity fix)" case_pinned_comm_name_never_adopts_selffile_identity
check "sot_jq_rawfile round-trips a leading-slash value through jq --rawfile" case_jq_rawfile_helper_round_trips_leading_slash_value
check "every jq --arg binding in the comm scripts + hooks is on the slash-safe allowlist" case_jq_arg_names_are_allowlisted_against_slash_prone_values
check "a slot claimed for another project refuses the join (exit 3), incumbent intact" case_slot_guard_refuses_another_projects_slot
check "--repin writes over another project's slot deliberately" case_slot_guard_repin_writes_anyway
check "a same-project rewrite is never refused" case_slot_guard_allows_a_same_project_rewrite
check "the shared nopane slot stays last-writer-wins" case_slot_guard_exempts_the_shared_nopane_slot
check "comm-self-audit.sh flags a slot naming another project and no suffixed/keyless one" case_self_audit_flags_only_a_slot_naming_another_project
check "the audit slugs a repo name with the daemon's own rule, not a second copy" case_self_audit_uses_the_daemons_own_slug_rule
check "the audit does not excuse a repo that suffixes the label (the other direction)" case_self_audit_does_not_excuse_a_repo_suffixing_the_label
check "a slot claimed during the join is refused by the writer with exit 3, not 1" case_slot_guard_refusal_in_the_write_gap_exits_three
check "sot_oneshot_request over a pipe: endpoint hands the bridge pipe:\\\\.\\pipe\\<name> and returns its matching reply (LU6e)" case_pipe_endpoint_oneshot_request_matches_reply
check "sot_oneshot_request over a pipe: endpoint fails cleanly with no sotd to open it (LU6e)" case_pipe_endpoint_oneshot_request_fails_cleanly_with_no_sotd
check "sot_oneshot_request over a pipe: endpoint names the bridge's own refusal (ADR 0049)" case_pipe_endpoint_oneshot_request_names_the_bridges_refusal
check "sot_daemon_endpoint on a simulated Windows host returns pipe: first and never calls pgrep (LU6e)" case_windows_pipe_discovery_returns_pipe_endpoint_and_skips_pgrep
check "sot_relay_endpoint on a simulated Windows host returns the binary's own answer, never the pipe the shell itself probed (C10)" case_windows_relay_endpoint_is_never_the_pipe_the_shell_probed

check "on Windows the sotd binary is SOTD_BIN or the install path, never a listed process's (ADR 0049)" case_windows_sotd_exe_is_never_a_listed_process

echo ""
echo "$PASS passed, $FAIL failed, $SKIP skipped"
[ "$FAIL" -eq 0 ]
