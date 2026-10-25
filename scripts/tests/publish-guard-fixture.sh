#!/bin/bash
# Sourced only by hermetic publish-guard suites. No product or account processes are started.
set -eu
set -o pipefail
ROOT=$(cd "${BASH_SOURCE[0]%/*}/../.." && pwd -P)
for pg_name in ${!GIT_@} ${!SOT_@} ${!CLAUDE_@} ${!CODEX_@}; do unset "$pg_name"; done
T=$(mktemp -d "${TMPDIR:-/tmp}/publish-guard.XXXXXX")
T=$(cd "$T" && pwd -P)
PG_WINDOWS=0
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) PG_WINDOWS=1 ;; esac
PG_READ_DENIED_FILE=
fixture_cleanup() {
    if [ -n "$PG_READ_DENIED_FILE" ]; then
        fixture_restore_read || { printf 'FAIL: cannot restore scratch file access\n' >&2; exit 1; }
    fi
    rm -rf "${T:?}"
}
trap fixture_cleanup EXIT
mkdir -p "$T/home" "$T/with spaces"
export HOME="$T/home" XDG_CONFIG_HOME="$T/home/config" XDG_STATE_HOME="$T/home/state"
# Native git.exe must receive real files, not an MSYS-only /dev/null pathname.
: > "$T/empty-global-config"; : > "$T/empty-system-config"
GIT_CONFIG_GLOBAL="$T/empty-global-config"; GIT_CONFIG_SYSTEM="$T/empty-system-config"
if [ "$PG_WINDOWS" = 1 ]; then
    GIT_CONFIG_GLOBAL=$(cygpath -am "$GIT_CONFIG_GLOBAL")
    GIT_CONFIG_SYSTEM=$(cygpath -am "$GIT_CONFIG_SYSTEM")
fi
export GIT_CONFIG_GLOBAL GIT_CONFIG_SYSTEM GIT_CONFIG_NOSYSTEM=1
export GIT_TERMINAL_PROMPT=0 GIT_ALLOW_PROTOCOL=file MSYS=winsymlinks:nativestrict
export GIT_AUTHOR_NAME=Fixture GIT_AUTHOR_EMAIL=fixture@example.invalid
export GIT_COMMITTER_NAME=Fixture GIT_COMMITTER_EMAIL=fixture@example.invalid
export GIT_AUTHOR_DATE='2020-01-01T00:00:00Z' GIT_COMMITTER_DATE='2020-01-01T00:00:00Z'
POLICY="$T/publish-list.txt"
printf '%s\n' '# synthetic policy' '' 'glimmerdeny' 'otherdeny' '!glimmerdeny.example/ok' > "$POLICY"
count=0
fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { count=$((count + 1)); printf 'ok %s: %s\n' "$count" "$1"; }
skip() { printf 'skip: %s\n' "$*"; }
fixture_executable() {
    # Windows script launch is proved by Git/marker observations, not a mode bit.
    [ "$PG_WINDOWS" = 1 ] || chmod +x "$@"
}
path_for_comparison() {
    # Compare path fields, never whole diagnostics or records whose bytes must be preserved.
    [ -n "$1" ] || return 1
    if command -v cygpath > /dev/null 2>&1; then
        cygpath -am -- "$1"
    else
        printf '%s\n' "$1"
    fi
}
fixture_link() { # kind, label, target, link; no copy may stand in for a link
    local kind=$1 label=$2 target=$3 link=$4 made=0 reason
    case "$link" in "$T"/*) ;; *) fail 'link fixture must stay under its scratch root' ;; esac
    if [ "$kind" = symlink ]; then
        if ln -s "$target" "$link" > "$T/link.out" 2>&1; then
            if [ "$PG_WINDOWS" = 1 ]; then
                if SOT_TEST_LINK="$(cygpath -aw "$link")" powershell.exe -NoProfile -NonInteractive -Command '
                    $ErrorActionPreference = "Stop"
                    $p = Get-Item -Force -LiteralPath $env:SOT_TEST_LINK
                    if (($p.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) { exit 1 }
                ' > "$T/link-native.out" 2>&1; then made=1; fi
            elif [ -L "$link" ]; then
                made=1
            fi
        fi
    elif [ "$kind" = hardlink ]; then
        if ln "$target" "$link" > "$T/link.out" 2>&1 && [ "$target" -ef "$link" ]; then made=1; fi
    else
        fail 'unknown link fixture kind'
    fi
    [ "$made" = 0 ] || return 0
    [ "$PG_WINDOWS" = 1 ] || fail "$label could not create a real $kind"
    rm -rf "${link:?}"
    if [ "$kind" = symlink ]; then
        reason='native symlinks require filesystem support and Developer Mode or symlink privilege'
    else
        reason='this Windows volume/runtime cannot establish hardlink file identity'
    fi
    skip "$label: $reason"
    return 1
}
fixture_restore_read() {
    local parent acl
    [ -n "$PG_READ_DENIED_FILE" ] || return 0
    if [ "$PG_WINDOWS" = 1 ]; then
        parent=$(cygpath -aw "${PG_READ_DENIED_FILE%/*}") || return 1
        acl=$(cygpath -aw "$T/read-acl") || return 1
        MSYS2_ARG_CONV_EXCL='*' icacls.exe "$parent" /restore "$acl" > "$T/read-restore.out" 2>&1 || return 1
    else
        chmod u+rw "$PG_READ_DENIED_FILE" || return 1
    fi
    "$BASH" -c ': < "$1"' fixture "$PG_READ_DENIED_FILE" > "$T/read-restored.out" 2>&1 || return 1
    PG_READ_DENIED_FILE=
}
fixture_deny_read() { # file, label; register restoration before changing access
    local file=$1 label=$2 sid native parent base acl
    case "$file" in "$T"/*) ;; *) fail 'read-denial fixture must stay under its scratch root' ;; esac
    [ -z "$PG_READ_DENIED_FILE" ] || fail 'a read-denial fixture is already active'
    if [ "$PG_WINDOWS" = 1 ]; then
        parent=${file%/*}; base=${file##*/}
        acl=$(cygpath -aw "$T/read-acl") || fail 'cannot resolve scratch ACL path'
        # Save a basename relative to its parent, so /restore touches that file alone.
        if ! (cd "$parent" && MSYS2_ARG_CONV_EXCL='*' icacls.exe "$base" /save "$acl") > "$T/read-save.out" 2>&1; then
            skip "$label: Windows cannot save the scratch file ACL on this volume/token"
            return 1
        fi
        if ! SOT_TEST_SID="$(cygpath -aw "$T/read-sid")" powershell.exe -NoProfile -NonInteractive -Command '
                $ErrorActionPreference = "Stop"
                [IO.File]::WriteAllText($env:SOT_TEST_SID, [Security.Principal.WindowsIdentity]::GetCurrent().User.Value)
            ' > "$T/read-identity.out" 2>&1; then
            skip "$label: Windows cannot resolve the scratch reader token"
            return 1
        fi
        sid=$(cat "$T/read-sid") || fail 'cannot read scratch ACL identity'
        case "$sid" in S-1-*) ;; *) fail 'invalid scratch ACL identity' ;; esac
        native=$(cygpath -aw "$file") || fail 'cannot resolve scratch ACL path'
        PG_READ_DENIED_FILE=$file
        # Avoid MSYS treating /deny as a pathname. Deny the full native Read mask.
        if ! MSYS2_ARG_CONV_EXCL='*' icacls.exe "$native" /deny "*${sid}:R" > "$T/read-deny.out" 2>&1; then
            fixture_restore_read || fail 'cannot restore scratch file access'
            skip "$label: Windows cannot apply an NTFS read deny on this volume/token"
            return 1
        fi
    else
        PG_READ_DENIED_FILE=$file
        chmod 000 "$file" || fail 'cannot deny scratch file reads'
    fi
}
payload() {
    mkdir -p "$1/scripts/hooks"
    cp "$ROOT/scripts/setup-publish-guard.sh" "$1/scripts/"
    cp "$ROOT/scripts/hooks/"* "$1/scripts/hooks/"
    if [ -n "${HOOK_SHELL:-}" ]; then
        for pg_hook in pre-push pre-push-shim; do
            { printf '#!%s\n' "$HOOK_SHELL"; tail -n +2 "$ROOT/scripts/hooks/$pg_hook"; } > "$1/scripts/hooks/$pg_hook"
        done
    fi
    fixture_executable "$1/scripts/hooks/pre-push" "$1/scripts/hooks/pre-push-shim" "$1/scripts/setup-publish-guard.sh"
}
fixture() {
    R="$T/with spaces/$1"; B="$T/with spaces/$1.git"
    git init -q --bare "$B"
    git init -q -b main "$R"
    git -C "$R" config commit.gpgsign false
    git -C "$R" config tag.gpgSign false
    git -C "$R" config core.autocrlf false
    git -C "$R" remote add origin "$B"
    payload "$R"
    printf 'clean\n' > "$R/data"
    git -C "$R" add .
    git -C "$R" commit -qm base
    "$BASH" "$R/scripts/setup-publish-guard.sh" "$R" --list "$POLICY" > "$T/setup.out" 2>&1 || fail 'fixture enrollment failed'
    # Optional prior-scanner anchor: a scratch copy plus synthetic adjacent list, never a live policy.
    if [ -n "${PUBLISH_GUARD_PRIOR_HOOK:-}" ]; then
        cp "$PUBLISH_GUARD_PRIOR_HOOK" "$R/.git/hooks/pre-push"
        cp "$POLICY" "$R/.git/scrub-patterns.txt"
        if [ -n "${HOOK_SHELL:-}" ]; then
            { printf '#!%s\n' "$HOOK_SHELL"; tail -n +2 "$PUBLISH_GUARD_PRIOR_HOOK"; } > "$R/.git/hooks/pre-push"
        fi
        fixture_executable "$R/.git/hooks/pre-push"
    fi
    git -C "$R" push -q origin main > "$T/push.out" 2>&1 || fail 'clean fixture push failed'
    BASE=$(git -C "$R" rev-parse HEAD)
}
commit() { git -C "$R" add -A; git -C "$R" commit -q --allow-empty -m "${1:-clean change}"; }
reset_case() { git -C "$R" reset -q --hard "$BASE"; }
refs() { git -C "$B" for-each-ref --format='%(refname) %(objectname)' | LC_ALL=C sort; }
audit_stderr() {
    if grep -iE 'glimmerdeny|otherdeny' "$1" > /dev/null; then fail "$2 leaked a policy entry"; fi
}
not_ready() { # readiness must reject for the intended reason, with sanitized stderr
    local label=$1 reason=$2 status=0
    "$BASH" "$R/scripts/setup-publish-guard.sh" "$R" --require-ready > "$T/ready.out" 2> "$T/ready.err" || status=$?
    [ "$status" -ne 0 ] || fail "$label reported ready"
    grep -F "$reason" "$T/ready.err" > /dev/null || fail "$label readiness wrong cause"
    audit_stderr "$T/ready.err" "$label readiness"
    ok "$label readiness"
}
reject() { # safe test label, expected hook diagnostic, push arguments
    local label=$1 reason=$2 before after push_status=0
    shift 2
    before=$(refs)
    git -C "$R" push -q "$@" > "$T/push.stdout" 2> "$T/push.err" || push_status=$?
    cat "$T/push.stdout" "$T/push.err" > "$T/push.out"
    if [ -n "${PG_FAULT_LOG:-}" ]; then
        [ -s "$PG_FAULT_LOG" ] || fail "$label fault injection did not reach the producer"
    fi
    [ "$push_status" -ne 0 ] || fail "$label accepted"
    after=$(refs)
    [ "$before" = "$after" ] || fail "$label changed destination refs"
    grep -F "$reason" "$T/push.out" > /dev/null || fail "$label did not reach its intended refusal"
    # Include utility/shell errors and chained stderr, even without a pre-push prefix.
    cp "$T/push.err" "$T/diagnostic"
    audit_stderr "$T/diagnostic" "$label"
    ok "$label"
}
accept() {
    local label=$1; shift
    git -C "$R" push -q "$@" > "$T/push.out" 2>&1 || fail "$label refused"
    # Every accepted explicit update must actually appear at the destination (not an up-to-date no-op).
    local arg destination source expected actual
    for arg in "$@"; do
        case "$arg" in -*|origin|/*|file://*) continue ;; esac
        case "$arg" in
            :*) destination=${arg#:}; git -C "$B" rev-parse --verify "$destination" > /dev/null 2>&1 && fail "$label deletion did not land" ;;
            *:*) source=${arg%%:*}; destination=${arg#*:}
                 expected=$(git -C "$R" rev-parse "$source")
                 actual=$(git -C "$B" rev-parse "$destination")
                 [ "$expected" = "$actual" ] || fail "$label destination differs" ;;
            *) expected=$(git -C "$R" rev-parse "$arg")
               actual=$(git -C "$B" rev-parse "$arg")
               [ "$expected" = "$actual" ] || fail "$label destination differs" ;;
        esac
    done
    ok "$label"
}
