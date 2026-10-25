#!/bin/bash
# Sourced only by hermetic publish-guard suites. No product or account processes are started.
set -eu
set -o pipefail
ROOT=$(cd "${BASH_SOURCE[0]%/*}/../.." && pwd -P)
for pg_name in ${!GIT_@} ${!SOT_@} ${!CLAUDE_@} ${!CODEX_@}; do unset "$pg_name"; done
T=$(mktemp -d "${TMPDIR:-/tmp}/publish-guard.XXXXXX")
T=$(cd "$T" && pwd -P)
trap 'rm -rf "${T:?}"' EXIT
mkdir -p "$T/home" "$T/with spaces"
export HOME="$T/home" XDG_CONFIG_HOME="$T/home/config" XDG_STATE_HOME="$T/home/state"
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_TERMINAL_PROMPT=0 GIT_ALLOW_PROTOCOL=file MSYS=winsymlinks:nativestrict
export GIT_AUTHOR_NAME=Fixture GIT_AUTHOR_EMAIL=fixture@example.invalid
export GIT_COMMITTER_NAME=Fixture GIT_COMMITTER_EMAIL=fixture@example.invalid
export GIT_AUTHOR_DATE='2020-01-01T00:00:00Z' GIT_COMMITTER_DATE='2020-01-01T00:00:00Z'
POLICY="$T/publish-list.txt"
printf '%s\n' '# synthetic policy' '' 'glimmerdeny' 'otherdeny' '!glimmerdeny.example/ok' > "$POLICY"
count=0
fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { count=$((count + 1)); printf 'ok %s: %s\n' "$count" "$1"; }
payload() {
    mkdir -p "$1/scripts/hooks"
    cp "$ROOT/scripts/setup-publish-guard.sh" "$1/scripts/"
    cp "$ROOT/scripts/hooks/"* "$1/scripts/hooks/"
    if [ -n "${HOOK_SHELL:-}" ]; then
        for pg_hook in pre-push pre-push-shim; do
            { printf '#!%s\n' "$HOOK_SHELL"; tail -n +2 "$ROOT/scripts/hooks/$pg_hook"; } > "$1/scripts/hooks/$pg_hook"
        done
    fi
    chmod +x "$1/scripts/hooks/pre-push" "$1/scripts/hooks/pre-push-shim" "$1/scripts/setup-publish-guard.sh"
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
        chmod +x "$R/.git/hooks/pre-push"
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
        case "$arg" in -*|origin) continue ;; esac
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
