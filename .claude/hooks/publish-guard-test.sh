#!/usr/bin/env bash
# Black-box tests for publish-guard.sh. Run: ./publish-guard-test.sh
#
# Fixtures are SYNTHETIC on purpose. The real denylist names the identifiers that
# must not appear in this public repo, so a test asserting against them would
# leak exactly what the guard exists to prevent. These drive the guard through
# $SOT_SCRUB_PATTERNS with a throwaway pattern file instead.
set -u
cd "$(dirname "$0")" || exit 1
G="$PWD/publish-guard.sh"
fails=0

pf="$(mktemp)"
cat > "$pf" <<'PATTERNS'
# synthetic denylist
secretname
badhost
/home/someuser
!github.com/secretname
PATTERNS
export SOT_SCRUB_PATTERNS="$pf"

check() { # check <want-exit> <label> <command-string>
    local want="$1" label="$2" cmd="$3" got
    printf '{"tool_name":"Bash","tool_input":{"command":"%s"}}' "$cmd" \
        | "$G" >/dev/null 2>&1
    got=$?
    if [ "$got" = "$want" ]; then
        printf '  ok    %s\n' "$label"
    else
        printf '  FAIL  (exit %s, want %s)  %s\n' "$got" "$want" "$label"
        fails=$((fails + 1))
    fi
}

echo "allow (exit 0):"
check 0 "non-publish command is never scanned"  'cargo test -p sot-frontend'
check 0 "clean commit message"                  'git commit -m \"fix: resolve paths\"'
check 0 "clean PR body"                         'gh pr create --body \"validated on the backend host\"'
check 0 "allow span rescues a public URL"       'gh pr create --body \"see github.com/secretname/repo\"'
check 0 "denied word inside a non-publish call" 'grep -r secretname .'
# Only the text the verb PUBLISHES is scanned. Commands are routinely prefixed
# with `cd <repo path>`, and that path can legitimately contain a denied span.
check 0 "cd prefix before verb is not scanned" 'cd /home/someuser/repo \&\& git commit -m \"clean subject\"'
check 0 "prefix path before a gh publish"      'cd /home/someuser/repo \&\& gh pr create --body \"clean body\"'

echo "block (exit 2):"
check 2 "name in PR body"                       'gh pr create --body \"ask secretname about it\"'
check 2 "case-insensitive match"                'gh pr create --body \"ask SecretName about it\"'
check 2 "second deny entry"                     'gh issue create --body \"deployed to BadHost\"'
check 2 "absolute path"                         'git commit -m \"see /home/someuser/x\"'
check 2 "bare name still denied despite allow"  'gh pr create --body \"github.com/secretname and secretname\"'
check 2 "issue comment surface"                 'gh issue comment 1 --body \"badhost again\"'
check 2 "release notes surface"                 'gh release create v1 --notes \"badhost\"'
check 2 "path INSIDE the message still blocks"  'cd /tmp \&\& git commit -m \"see /home/someuser/x\"'

# Unconfigured vs misconfigured. A public cloner (no denylist, no marker) must get
# a silent working repo. A machine that ASSERTS it should be guarded but resolves
# nothing must block — that state used to be indistinguishable from a protected one.
PUB='{"tool_name":"Bash","tool_input":{"command":"gh pr create --body \"secretname\""}}'
env_check() { # env_check <want-exit> <label> <env-assignments...>
    local want="$1" label="$2"; shift 2
    ( unset SOT_SCRUB_PATTERNS SOT_OPS_DIR CLAUDE_PROJECT_DIR
      export "$@" 2>/dev/null
      printf '%s' "$PUB" | "$G" >/dev/null 2>&1 )
    local got=$?
    if [ "$got" = "$want" ]; then printf '  ok    %s\n' "$label"
    else printf '  FAIL  (exit %s, want %s)  %s\n' "$got" "$want" "$label"; fails=$((fails + 1)); fi
}

echo "unconfigured (exit 0 — a public clone is never blocked or nagged):"
bare="$(mktemp -d)"; mkdir -p "$bare/proj"     # no sibling ops sidecar
trap 'rm -f "$pf"; rm -rf "$bare" "$stale"' EXIT
env_check 0 "no markers at all"                  CLAUDE_PROJECT_DIR="$bare/proj"
env_check 0 "no project dir either"              DUMMY=1

echo "misconfigured (exit 2 — expected but inert must be LOUD, not silent):"
stale="$(mktemp -d)"; mkdir -p "$stale/proj" "$stale/ship-of-tools-ops"  # sidecar, no denylist
env_check 2 "sidecar exists but denylist missing" CLAUDE_PROJECT_DIR="$stale/proj"
env_check 2 "SOT_OPS_DIR set but unresolvable"    SOT_OPS_DIR=/nonexistent
env_check 2 "SOT_SCRUB_PATTERNS set but missing"  SOT_SCRUB_PATTERNS=/nonexistent/patterns.txt

echo "windows path normalisation:"
# ${proj%/*} cannot strip a backslash, so an un-normalised native path resolved the
# sidecar INSIDE the project dir and silently missed. Assert the sidecar is FOUND.
win="$(mktemp -d)"; mkdir -p "$win/proj" "$win/ship-of-tools-ops"
cp "$pf" "$win/ship-of-tools-ops/scrub-patterns.txt"
env_check 2 "backslash project dir still resolves sidecar" \
    CLAUDE_PROJECT_DIR="$(printf '%s' "$win/proj" | tr '/' '\\')"
rm -rf "$win"

echo "readiness check (--check):"
out="$( ( unset SOT_SCRUB_PATTERNS SOT_OPS_DIR
          CLAUDE_PROJECT_DIR="$stale/proj" "$G" --check ) 2>/dev/null )"
case "$out" in
    *systemMessage*INERT*) printf '  ok    --check reports an inert guard\n' ;;
    *) printf '  FAIL  --check must report an inert guard (got: %s)\n' "${out:-<empty>}"; fails=$((fails + 1)) ;;
esac
out="$( ( unset SOT_OPS_DIR CLAUDE_PROJECT_DIR
          SOT_SCRUB_PATTERNS="$pf" "$G" --check ) 2>/dev/null )"
if [ -z "$out" ]; then printf '  ok    --check is silent when guarded\n'
else printf '  FAIL  --check must stay silent when guarded (got: %s)\n' "$out"; fails=$((fails + 1)); fi

echo
if [ "$fails" -eq 0 ]; then echo "publish-guard: ALL PASS"; else echo "publish-guard: $fails FAILURE(S)"; fi
exit "$fails"
