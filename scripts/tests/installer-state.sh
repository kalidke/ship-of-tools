#!/usr/bin/env bash
# installer-state.sh — the decision matrix for "an install is already here".
#
# install.json recorded the role of every completed install and nothing ever
# read it back, so a re-run on a machine with a live backend silently re-roled
# it. These tests pin the gate that stops that, and the two limits that shape
# it: the manifest fails CLOSED when it cannot be read (uncertainty is not
# permission), and a `remote` role is never reused automatically because
# schema 1 records no ssh alias to reuse.
#
# Separate from hosts-toml-role.sh on purpose — that file has a narrow contract
# about editing one config file.
#
# Run: scripts/tests/installer-state.sh

set -euo pipefail

SOT_INSTALL_SOURCE_ONLY=1
export SOT_INSTALL_SOURCE_ONLY
# shellcheck source=../install.sh
. "$(dirname "$0")/../install.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
fails=0

check() {  # <description> <expected> <actual>
    if [ "$2" = "$3" ]; then
        printf '    ok   %s\n' "$1"
    else
        printf '    FAIL %s\n      expected: %s\n      actual:   %s\n' "$1" "$2" "$3"
        fails=$((fails + 1))
    fi
}
starts_with() {  # <description> <prefix> <actual>
    case "$3" in
        "$2"*) printf '    ok   %s\n' "$1" ;;
        *) printf '    FAIL %s\n      expected prefix: %s\n      actual:          %s\n' "$1" "$2" "$3"
           fails=$((fails + 1)) ;;
    esac
}
case_start() { printf '  %s\n' "$1"; }

manifest() {  # <dir> <json-body>
    mkdir -p "$1"
    printf '%s' "$2" > "$1/install.json"
}

command -v jq >/dev/null 2>&1 || {
    printf 'installer-state: jq is required to run these tests\n' >&2
    exit 1
}

# ---------------------------------------------------------------------------
case_start "reading the manifest"

d="$WORK/absent"; mkdir -p "$d"
check "no manifest is 'none'" "none" "$(installer_manifest_state "$d")"

d="$WORK/good"
manifest "$d" "$(printf '{"schema":1,"role":"be-only","prefix":"%s"}' "$d")"
check "schema 1 yields the role" "known:be-only" "$(installer_manifest_state "$d")"

d="$WORK/noprefix"
manifest "$d" '{"schema":1,"role":"local"}'
check "a manifest with no prefix still reads" "known:local" "$(installer_manifest_state "$d")"

# Fails closed from here down: every one of these must be 'unknown', never
# 'none'. Reading them as 'none' is what would grant permission to reconfigure.
d="$WORK/truncated"
manifest "$d" '{"schema": 1, "role": "be-only", "prefi'
starts_with "a truncated manifest is unknown, not absent" "unknown:" "$(installer_manifest_state "$d")"

d="$WORK/garbage"
manifest "$d" 'this is not json at all'
starts_with "garbage is unknown" "unknown:" "$(installer_manifest_state "$d")"

d="$WORK/empty"
manifest "$d" ''
starts_with "an empty file is unknown" "unknown:" "$(installer_manifest_state "$d")"

d="$WORK/future"
manifest "$d" '{"schema":2,"role":"local"}'
starts_with "a newer schema is unknown, not absent" "unknown:" "$(installer_manifest_state "$d")"

d="$WORK/badrole"
manifest "$d" '{"schema":1,"role":"something-else"}'
starts_with "an unrecognized role is unknown" "unknown:" "$(installer_manifest_state "$d")"

d="$WORK/moved"
manifest "$d" '{"schema":1,"role":"local","prefix":"/somewhere/else"}'
starts_with "a manifest recording another prefix is unknown" "unknown:" "$(installer_manifest_state "$d")"

# ---------------------------------------------------------------------------
case_start "the gate"

check "a fresh machine is allowed" \
    "allow" "$(installer_role_decision none "" local 0)"
check "the same role again is an upgrade" \
    "allow" "$(installer_role_decision known:be-only be-only be-only 0)"

# The incident: a documented one-liner carrying a role flag, run on a box that
# already had a backend.
starts_with "a DIFFERENT role flag is refused" \
    "refuse:" "$(installer_role_decision known:be-only be-only local 0)"
starts_with "the refusal names the flag that would do it" \
    "refuse:this machine is already installed as be-only, and --local" \
    "$(installer_role_decision known:be-only be-only local 0)"
check "--force-role-change is consent" \
    "allow" "$(installer_role_decision known:be-only be-only local 1)"

starts_with "an unreadable manifest refuses" \
    "refuse:" "$(installer_role_decision 'unknown:whatever' "" local 0)"
check "and --force-role-change still overrides it" \
    "allow" "$(installer_role_decision 'unknown:whatever' "" local 1)"

# No role flag means the interactive Q&A has not run. Picking a visibly
# non-current option there is the explicit choice, so the gate stays out of it.
check "no flag defers to the interactive prompt" \
    "allow" "$(installer_role_decision known:be-only be-only "" 0)"

# ---------------------------------------------------------------------------
case_start "no automatic reuse of a remote role"
# Schema 1 stores no ssh alias or port, so 'keep what was recorded' would
# produce ROLE=remote with an empty BE_ALIAS — a broken [host.] entry and a
# broken launcher. The gate must refuse a change, never silently reinstate one.
check "a recorded remote role is not reused for another flag" \
    "allow" "$(installer_role_decision known:remote remote remote 0)"
starts_with "and switching away from it is still refused" \
    "refuse:" "$(installer_role_decision known:remote remote be-only 0)"
check "the manifest exposes no alias to reuse" \
    "" "$(jq -r '.ssh_alias // ""' <<<'{"schema":1,"role":"remote"}')"

# ---------------------------------------------------------------------------
case_start "unit ownership: ExecStart path extraction (old + wrapped forms)"
# Codex round on PR #164: installer_unit_owner_path used to take the FIRST
# WORD of ExecStart, which was the sotd path in the old direct form but
# became "/bin/bash" once ExecStart wraps the daemon in a shell that sources
# ~/.bashrc (deploy/sotd.service). A reinstall then treated its own unit as
# foreign and aborted (or demanded --force-role-change) every time. Pin both
# shapes so the regex never regresses to first-word-only again.

old_unit="$(cat <<'UNITEOF'
ExecStartPre=-/opt/sot/bin/sot-apply
ExecStart=/opt/sot/bin/sotd --project-root /home/u --label sot
Restart=always
UNITEOF
)"
check "old direct ExecStart form" \
    "/opt/sot/bin/sotd" "$(printf '%s\n' "$old_unit" | installer_unit_owner_path)"

wrapped_unit="$(cat <<'UNITEOF'
ExecStartPre=-/opt/sot/bin/sot-apply
ExecStart=/bin/bash -c '[ -r "$HOME/.bashrc" ] && . "$HOME/.bashrc"; exec "/opt/sot/bin/sotd" --project-root "/home/u" --label sot'
Restart=always
UNITEOF
)"
check "wrapped bash -c ExecStart form (current unit)" \
    "/opt/sot/bin/sotd" "$(printf '%s\n' "$wrapped_unit" | installer_unit_owner_path)"

no_execstart="$(cat <<'UNITEOF'
[Unit]
Description=something else entirely
UNITEOF
)"
check "no ExecStart line yields empty" \
    "" "$(printf '%s\n' "$no_execstart" | installer_unit_owner_path)"

# ---------------------------------------------------------------------------
printf '\n'
if [ "$fails" -eq 0 ]; then
    printf 'installer-state: all checks passed\n'
else
    printf 'installer-state: %d check(s) FAILED\n' "$fails" >&2
    exit 1
fi
