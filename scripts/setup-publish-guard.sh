#!/bin/bash
# One owner for clone enrollment. Installer/provisioning supplies --list; development only ensures placement.
set -u
set -o pipefail
fail() { printf 'publish guard: %s\n' "$1" >&2; exit 1; }
installer=0
conflict() { printf 'publish guard: %s\n' "$1" >&2; [ "$installer" = 0 ] && exit 1; exit 4; }
[ $# -ge 1 ] || fail 'usage: setup-publish-guard.sh CHECKOUT [--list ABSOLUTE_PATH] [--require-ready] [--installer] [--check-only]'
here=$(cd "${BASH_SOURCE[0]%/*}" && pwd -P) || fail 'cannot resolve setup source'
checkout=$1; shift
policy= supplied=0 ready=0 check=0
while [ $# -gt 0 ]; do
    case "$1" in
        --list) [ $# -ge 2 ] || fail '--list needs an absolute path'; policy=$2; supplied=1; shift 2 ;;
        --require-ready) ready=1; shift ;;
        --installer) installer=1; ready=1; shift ;;
        --check-only) check=1; shift ;;
        *) fail 'unknown setup argument' ;;
    esac
done
[ "$installer" = 0 ] || [ "$supplied" = 1 ] || fail '--installer needs --list'
cd "$checkout" || fail 'cannot enter checkout'
checkout=$(pwd -P) || fail 'cannot resolve checkout'
# Discover from the named directory, never from a parent Git process's repository selectors.
unset GIT_DIR GIT_COMMON_DIR GIT_WORK_TREE GIT_IMPLICIT_WORK_TREE GIT_INDEX_FILE GIT_OBJECT_DIRECTORY \
    GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_NAMESPACE GIT_PREFIX GIT_SHALLOW_FILE GIT_GRAFT_FILE \
    GIT_REPLACE_REF_BASE GIT_CONFIG GIT_CONFIG_PARAMETERS GIT_CONFIG_COUNT GIT_CEILING_DIRECTORIES \
    GIT_DISCOVERY_ACROSS_FILESYSTEM
selectors=$(git rev-parse --local-env-vars 2>/dev/null) || fail 'cannot inspect Git repository selectors'
for selector in $selectors; do unset "$selector"; done
common=$(git rev-parse --git-common-dir 2>/dev/null) || fail 'checkout is not a Git repository'
common=$(cd "$common" && pwd -P) || fail 'cannot resolve common Git directory'
# The setup source may differ from the checkout being enrolled.
[ -f "$here/hooks/publish-guard-lib.sh" ] && [ -r "$here/hooks/publish-guard-lib.sh" ] || fail 'cannot load policy validator'
. "$here/hooks/publish-guard-lib.sh" 2>/dev/null || fail 'cannot load policy validator'
for validator in pg_fail pg_list pg_list_encoding pg_no_reparse; do
    declare -F "$validator" > /dev/null || fail 'policy validator is incomplete'
done
source_hook="$here/hooks/pre-push-shim"
[ -f "$source_hook" ] && [ ! -L "$source_hook" ] || fail 'shipped hook shim is missing'
git config --show-origin --get core.hooksPath > /dev/null 2>&1; status=$?
if [ "$status" = 0 ]; then
    # Any explicit setting is preserved, even an empty setting or a worktree/global override.
    printf 'publish guard: existing hooks configuration kept for %s\n' "$checkout" >&2
    printf 'publish guard: inspect preserved hooks, then migrate the clone-local override with: git -C %q config --local --unset-all core.hooksPath\n' "$checkout" >&2
    conflict 'effective core.hooksPath must be unset before enrollment; global/worktree overrides also need their owner'
fi
[ "$status" = 1 ] || fail 'cannot inspect effective core.hooksPath'
for dir in "$common/hooks" "$common/publish-guard"; do
    if [ -e "$dir" ]; then pg_no_reparse "$dir" || conflict 'hook or ownership directory is an unresolved alias'; fi
    [ ! -L "$dir" ] || conflict 'hook or ownership directory is a symlink'
    [ ! -e "$dir" ] || [ -d "$dir" ] || conflict 'hook or ownership directory is not a directory'
done
slot="$common/hooks/pre-push"
record="$common/publish-guard/installed-hash"
for file in "$slot" "$record"; do
    if [ -e "$file" ]; then pg_no_reparse "$file" || conflict 'hook slot or ownership record is an unresolved alias'; fi
done
[ ! -L "$slot" ] && [ ! -L "$record" ] || conflict 'hook slot or ownership record is a symlink'
[ ! -e "$record" ] || [ -f "$record" ] || conflict 'ownership record is not a regular file'
if [ -e "$slot" ]; then
    [ -f "$slot" ] || conflict 'pre-push slot is not a regular file'
    if ! cmp -s "$slot" "$source_hook"; then
        [ -f "$record" ] || conflict 'occupied pre-push slot has no ownership record; preserve it explicitly as pre-push.local'
        installed=$(cat "$record") || fail 'cannot read ownership record'
        case "$installed" in ''|*[!0-9a-f]*) conflict 'invalid ownership record' ;; esac
        actual=$(git hash-object --no-filters "$slot") || fail 'cannot identify installed hook'
        [ "$actual" = "$installed" ] || conflict 'locally modified pre-push hook kept; inspect it before enrollment'
    fi
fi
# Checking never repairs the state it observes. Publication scratch stays on the destination filesystem.
if [ "$check" = 0 ]; then
    mkdir -p "$common/hooks" "$common/publish-guard" || fail 'cannot create hook directories'
    template="$common/publish-guard/setup.XXXXXX"
else
    template="${TMPDIR:-/tmp}/publish-guard-check.XXXXXX"
fi
scratch=$(mktemp -d "$template") || fail 'cannot create setup scratch directory'
trap 'rm -rf "${scratch:?}"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM HUP
if [ "$check" = 0 ]; then
    cp "$source_hook" "$scratch/pre-push" && chmod +x "$scratch/pre-push" || fail 'cannot prepare hook shim'
    digest=$(git hash-object --no-filters "$scratch/pre-push") || fail 'cannot identify shipped hook'
    printf '%s\n' "$digest" > "$scratch/installed-hash" || fail 'cannot prepare ownership record'
    mv -f "$scratch/pre-push" "$slot" || fail 'cannot publish hook shim'
    mv -f "$scratch/installed-hash" "$record" || fail 'cannot publish ownership record'
    if [ "$supplied" = 1 ]; then
        case "$policy" in /*|[a-zA-Z]:[\\/]*) ;; *) fail 'sot.publishList must be absolute' ;; esac
        git config --local --replace-all sot.publishList "$policy" || fail 'cannot record sot.publishList'
    fi
fi
if [ "$check" = 1 ]; then
    [ -f "$slot" ] && [ -f "$record" ] && cmp -s "$slot" "$source_hook" || fail 'installed hook or ownership record is incomplete'
    digest=$(git hash-object --no-filters "$slot") || fail 'cannot identify installed hook'
    installed=$(cat "$record") || fail 'cannot read ownership record'
    [ "$digest" = "$installed" ] || fail 'installed ownership record does not match'
    if [ "$supplied" = 1 ]; then
        git config --local --no-includes --null --get-all sot.publishList > "$scratch/actual" 2>/dev/null || fail 'cannot read recorded sot.publishList'
        printf '%s\0' "$policy" > "$scratch/expected" || fail 'cannot prepare location check'
        cmp -s "$scratch/actual" "$scratch/expected" || fail 'recorded sot.publishList changed'
    fi
fi
# Check effective selection after publication too; never set core.hooksPath.
git config --get core.hooksPath > /dev/null 2>&1; status=$?
[ "$status" = 1 ] && [ -x "$slot" ] || fail 'effective hook selection is not the common default slot'
if [ "$check" = 1 ]; then printf 'publish guard: checked %s\n' "$checkout"
else printf 'publish guard: enrolled %s\n' "$checkout"; fi
if [ "$ready" = 1 ]; then
    if [ "$installer" = 1 ]; then
        # Keep policy repairs distinct from a failed Git/scanner producer. No policy text or path is printed.
        pg_fail() {
            printf 'publish guard: %s\n' "$1" >&2
            case "$1" in
                'sot.publishList is unset or unreadable'|'cannot validate sot.publishList encoding'|'cannot inspect Git environment'|'sot.publishList repository check failed'|'sot.publishList encoding validation failed') exit 1 ;;
                *) exit 3 ;;
            esac
        }
    fi
    pg_tmp=$scratch
    export LC_ALL=C
    pg_list || fail 'policy validation failed'
    printf 'publish guard: policy ready\n'
else
    printf 'publish guard: pushes require one valid external clone-local sot.publishList\n'
fi
