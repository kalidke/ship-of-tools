#!/usr/bin/env bash
# under-bash.sh BASH [-- SUITE...] -- the shell scripts under BASH, the bash 3.2 the records promise (comm/CLAUDE.md,
# scripts/CLAUDE.md). First every tracked shell script outside a tests folder under comm/, agents/ and scripts/ must
# parse under BASH. Then each SUITE (a path from the repository root) runs from its own folder with a folder holding
# `bash -> BASH` first on PATH, so every script it starts through `#!/usr/bin/env bash`, `bash x.sh` or `bash -c` gets
# BASH (a `#!/bin/bash` script gets it only where BASH is /bin/bash, as on macOS). A suite under scripts/tests sources
# the scripts it tests into its own shell, so it runs on BASH itself; any other suite runs on the bash running this
# script, which must be 4 or later, since those suites use what 3.2 lacks. With no SUITE: every hermetic suite, each
# comm/tests/test-*.sh and agents/tests/test-*.sh but the five that need peer hosts (comm/tests/CLAUDE.md), and the
# five scripts/tests suites. Prints `under-bash: PASS|FAIL SUITE` per suite and `under-bash: FAIL parse SCRIPT` per
# script that does not parse or runs a utility by the `command` builtin outside a pipeline (under bash 3.2 and set -e
# that ends the shell when the utility fails, even where an if, ! or || handles it); exits with the count of FAIL
# lines, or 2 when the setup is wrong.
set -u
[ $# -eq 1 ] || { [ $# -ge 3 ] && [ "$2" = -- ]; } || { echo "usage: under-bash.sh BASH [-- SUITE...]" >&2; exit 2; }
want="$1"; shift; [ $# -eq 0 ] || shift
[ "${BASH_VERSINFO[0]}" -ge 4 ] || { echo "under-bash.sh: run it with bash 4 or later, not $BASH_VERSION" >&2; exit 2; }
cd "$(git rev-parse --show-toplevel)" || exit 2
if [ $# -eq 0 ]; then
    for s in comm/tests/test-*.sh agents/tests/test-*.sh; do
        case "${s##*/}" in
            test-comm-e2e-readers.sh|test-inbox-lock-onehost.sh|test-inbox-lock-twohost.sh|test-registry-twohost.sh) ;;
            test-registry-lock-twohost.sh) ;;
            *) set -- "$@" "$s" ;;
        esac
    done
    set -- "$@" scripts/tests/installer-state.sh scripts/tests/installer-wrapper.sh scripts/tests/installer-apply.sh \
        scripts/tests/installer-shared-home.sh scripts/tests/test-topology-plan.sh
fi
shadow="$(mktemp -d "${TMPDIR:-/tmp}/under-bash-XXXXXX")" || exit 2
trap 'rm -rf "${shadow:?}"' EXIT
ln -s "$want" "$shadow/bash" || exit 2
export PATH="$shadow:$PATH"
v="$("$want" -c 'echo "$BASH_VERSION"')" && [ "$(bash -c 'echo "$BASH_VERSION"')" = "$v" ] \
    || { echo "under-bash.sh: bash on PATH is not $want" >&2; exit 2; }
echo "under-bash: scripts run by bash $v; suites outside scripts/tests by bash $BASH_VERSION"
failed=0 n=0
while IFS= read -r f; do
    case "$f" in */tests/*) continue ;; esac
    case "$f" in *.sh) ;; *) l=""; IFS= read -r l < "$f"; case "$l" in '#!'*bash*) ;; *) continue ;; esac ;; esac
    n=$((n + 1))
    "$want" -n "$f" || { echo "under-bash: FAIL parse $f"; failed=$((failed + 1)); }
    # A pipeline stage runs in its own process and is safe (comm-lib-base.sh's sot_jq); `command -v` runs nothing.
    while IFS= read -r hit; do
        echo "under-bash: FAIL command-builtin $f:$hit"; failed=$((failed + 1))
    done < <(grep -nE '(^|[;&|{!]|\$\(|then|do|else|if|while|until) *command +[^-[:space:]]' "$f" | grep -vE '^[0-9]+:[[:space:]]*#' \
                 | grep -vE 'command +[^|;&]*[|][^|]' || true)
done < <(git ls-files -- comm agents scripts)
echo "under-bash: parsed $n scripts"
for s in "$@"; do
    echo "under-bash: run $s"
    case "$s" in scripts/tests/*) by="$want" ;; *) by="$BASH" ;; esac
    ( cd "${s%/*}" && "$by" "./${s##*/}" ); rc=$?
    if [ "$rc" -eq 0 ]; then echo "under-bash: PASS $s"; else echo "under-bash: FAIL $s (rc $rc)"; failed=$((failed + 1)); fi
done
exit "$failed"
