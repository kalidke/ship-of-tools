#!/usr/bin/env bash
# under-bash.sh BASH [--parse-only|--no-parse] [-- SUITE...] -- the shell scripts under BASH, the bash 3.2 the records promise (comm/CLAUDE.md,
# scripts/CLAUDE.md). First every tracked shell script outside a tests folder under comm/, agents/ and scripts/ must
# parse under BASH. Then each SUITE (a path from the repository root) runs from its own folder with a folder holding
# `bash -> BASH` first on PATH, so every script it starts through `#!/usr/bin/env bash`, `bash x.sh` or `bash -c` gets
# BASH (the publish suites rewrite only scratch hook shebangs through HOOK_SHELL; other `#!/bin/bash` scripts get it
# only where BASH is /bin/bash, as on macOS). A suite under scripts/tests sources
# the scripts it tests into its own shell, so it runs on BASH itself; any other suite runs on the bash running this
# script, which must be 4 or later, since those suites use what 3.2 lacks. With no SUITE: every hermetic suite, each
# comm/tests/test-*.sh and agents/tests/test-*.sh, and the explicit scripts/tests suites. Prints `under-bash: PASS|FAIL SUITE` per suite and `under-bash: FAIL parse SCRIPT` per
# script that does not parse or runs a utility by the `command` builtin outside a pipeline (under bash 3.2 and set -e
# that ends the shell when the utility fails, even where an if, ! or || handles it), and `under-bash: FAIL early-reader
# SCRIPT:LINE:TEXT` per pipeline whose reader can stop early (sigpipe-scan.awk), and `under-bash: FAIL bare-in-place-sed
# FILE:LINE:TEXT` per in-place sed with no suffix in any tracked file under comm/, agents/ or scripts/, tests included
# (macOS's sed takes the expression as the suffix); exits with the count of FAIL lines, or 2 when the setup is wrong.
# `--parse-only` after BASH runs the scans and no suite; `--no-parse` runs the suites and no scan (rc-gate.sh runs the
# scans as one job and each suite as another); `--list` alone prints the default suites, one path per line.
set -u
suites() {
    printf '%s\n' comm/tests/test-*.sh agents/tests/test-*.sh
    printf '%s\n' scripts/tests/installer-state.sh scripts/tests/installer-wrapper.sh scripts/tests/installer-apply.sh \
        scripts/tests/installer-shared-home.sh scripts/tests/test-topology-plan.sh scripts/tests/test-sigpipe-scan.sh \
        scripts/tests/test-release-cut.sh scripts/tests/test-pre-push.sh scripts/tests/test-publish-guard-setup.sh \
        scripts/tests/test-publish-guard-entries.sh
}
if [ "${1:-}" = --list ] && [ $# -eq 1 ]; then cd "$(dirname "$0")/../.." && suites; exit; fi
[ $# -ge 1 ] || { echo "usage: under-bash.sh BASH [--parse-only|--no-parse] [-- SUITE...]" >&2; exit 2; }
want="$1"; shift
mode=all
case "${1:-}" in --parse-only) mode=parse; shift ;; --no-parse) mode=suites; shift ;; esac
[ $# -eq 0 ] || [ "$1" = -- ] || { echo "usage: under-bash.sh BASH [--parse-only|--no-parse] [-- SUITE...]" >&2; exit 2; }
[ $# -eq 0 ] || shift
[ "${BASH_VERSINFO[0]}" -ge 4 ] || { echo "under-bash.sh: run it with bash 4 or later, not $BASH_VERSION" >&2; exit 2; }
cd "$(git rev-parse --show-toplevel)" || exit 2
if [ "$mode" = parse ]; then set --; elif [ $# -eq 0 ]; then
    while IFS= read -r s; do set -- "$@" "$s"; done < <(suites)
fi
shadow="$(mktemp -d "${TMPDIR:-/tmp}/under-bash-XXXXXX")" || exit 2
trap 'rm -rf "${shadow:?}"' EXIT
ln -s "$want" "$shadow/bash" || exit 2
export PATH="$shadow:$PATH"
# Git launches scratch copies of the persistent shim and guard under this exact shell.
export HOOK_SHELL="$want"
v="$("$want" -c 'echo "$BASH_VERSION"')" && [ "$(bash -c 'echo "$BASH_VERSION"')" = "$v" ] \
    || { echo "under-bash.sh: bash on PATH is not $want" >&2; exit 2; }
echo "under-bash: scripts run by bash $v; suites outside scripts/tests by bash $BASH_VERSION"
failed=0 n=0
while IFS= read -r f; do
    [ "$mode" != suites ] || break
    # An in-place sed without a suffix fails on macOS: BSD sed reads the expression as the suffix. Write to a temporary
    # file and move it over, as release.sh does.
    while IFS= read -r hit; do
        echo "under-bash: FAIL bare-in-place-sed $f:$hit"; failed=$((failed + 1))
    done < <(grep -nIE '(^|[^[:alnum:]_])sed( +-[[:alpha:]]+)* +(--in-place(=[^ ]*)?|-[[:alpha:]]*i)( |$)' "$f" || true)
    case "$f" in */tests/*) continue ;; esac
    case "$f" in *.sh) ;; *) l=""; IFS= read -r l < "$f"; case "$l" in '#!'*bash*) ;; *) continue ;; esac ;; esac
    n=$((n + 1))
    "$want" -n "$f" || { echo "under-bash: FAIL parse $f"; failed=$((failed + 1)); }
    # A pipeline stage runs in its own process and is safe (comm-lib-base.sh's sot_jq); `command -v` runs nothing.
    while IFS= read -r hit; do
        echo "under-bash: FAIL command-builtin $f:$hit"; failed=$((failed + 1))
    done < <(grep -nE '(^|[;&|{!]|\$\(|then|do|else|if|while|until) *command +[^-[:space:]]' "$f" | grep -vE '^[0-9]+:[[:space:]]*#' \
                 | grep -vE 'command +[^|;&]*[|][^|]' || true)
    # A reader that stops early (head, grep -q, awk exit) fails its pipeline at random under pipefail (sigpipe-scan.awk).
    while IFS= read -r hit; do
        echo "under-bash: FAIL early-reader ${hit:0:200}"; failed=$((failed + 1))
    done < <(awk -f "$PWD/scripts/tests/sigpipe-scan.awk" "$f")
done < <(git ls-files -- comm agents scripts)
[ "$mode" = suites ] || echo "under-bash: parsed $n scripts"
for s in "$@"; do
    echo "under-bash: run $s"
    case "$s" in scripts/tests/*) by="$want" ;; *) by="$BASH" ;; esac
    ( cd "${s%/*}" && "$by" "./${s##*/}" ); rc=$?
    if [ "$rc" -eq 0 ]; then echo "under-bash: PASS $s"; else echo "under-bash: FAIL $s (rc $rc)"; failed=$((failed + 1)); fi
done
exit "$failed"
