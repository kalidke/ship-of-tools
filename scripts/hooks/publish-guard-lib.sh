#!/bin/bash
# Shared policy-location validation. Diagnostics deliberately omit the configured path.
pg_fail() { printf 'pre-push: %s; refusing the push\n' "$1" >&2; exit 1; }
pg_no_reparse() {
    local native host_system
    host_system=$(uname -s) || return 1
    case "$host_system" in
        Linux|Darwin) ;;
        MINGW*|MSYS*|CYGWIN*)
            native=$(cygpath -aw "$1" 2>/dev/null) || return 1
            SOT_POLICY_PATH="$native" powershell.exe -NoLogo -NoProfile -NonInteractive -Command '
                $ErrorActionPreference = "Stop"
                try {
                    $p = Get-Item -Force -LiteralPath $env:SOT_POLICY_PATH
                    while ($null -ne $p) {
                        if (($p.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { exit 1 }
                        if ($p -is [IO.FileInfo]) { $p = $p.Directory } else { $p = $p.Parent }
                    }
                } catch { exit 1 }
                exit 0' > /dev/null 2>&1 || return 1 ;;
        *) return 1 ;;
    esac
    return 0
}
pg_list_encoding() {
    # ASCII text only: printable bytes, TAB, LF and CR. In particular no NUL, BOM or UTF-16.
    { LC_ALL=C tr -d '\011\012\015\040-\176' < "$pg_tmp/list" > "$pg_tmp/invalid-encoding"; } 2>/dev/null ||
        pg_fail 'cannot validate sot.publishList encoding'
    [ ! -s "$pg_tmp/invalid-encoding" ] || pg_fail 'sot.publishList must be ASCII text without a BOM'
}
pg_list() {
    local value extra parent probe rc names name
    git config --local --no-includes --null --get-all sot.publishList > "$pg_tmp/config" 2>/dev/null ||
        pg_fail 'sot.publishList is unset or unreadable'
    exec 3< "$pg_tmp/config"
    IFS= read -r -d '' value <&3 || pg_fail 'sot.publishList is invalid'
    if IFS= read -r -d '' extra <&3; then pg_fail 'sot.publishList has multiple values'; fi
    exec 3<&-
    case "$value" in
        /*) ;;
        [a-zA-Z]:[\\/]*)
            value=$(cygpath -u "$value" 2>/dev/null) || pg_fail 'sot.publishList path conversion failed' ;;
        *) pg_fail 'sot.publishList must be absolute' ;;
    esac
    [ ! -L "$value" ] && [ -f "$value" ] && [ -r "$value" ] ||
        pg_fail 'sot.publishList must name a readable regular file without a symlink'
    pg_no_reparse "$value" || pg_fail 'sot.publishList has an unresolved directory alias'
    parent=${value%/*}; [ -n "$parent" ] || parent=/
    parent=$(cd "$parent" 2>/dev/null && pwd -P) || pg_fail 'sot.publishList parent is unreadable'
    # Repository selectors must not redirect the independent containment probe back to the pushing clone.
    names=$(git rev-parse --local-env-vars 2>/dev/null) || pg_fail 'cannot inspect Git environment'
    (
        for name in $names; do unset "$name"; done
        unset GIT_CONFIG GIT_CONFIG_PARAMETERS GIT_CONFIG_COUNT GIT_CEILING_DIRECTORIES GIT_DISCOVERY_ACROSS_FILESYSTEM
        export GIT_DISCOVERY_ACROSS_FILESYSTEM=1
        git -C "$parent" rev-parse --absolute-git-dir > "$pg_tmp/probe" 2> "$pg_tmp/probe-error"
    ) && pg_fail 'sot.publishList is inside a repository'
    rc=$?
    # A successful probe exits above. A failed discovery must be the ordinary outside-repository result.
    [ "$rc" -eq 128 ] || pg_fail 'sot.publishList repository check failed'
    LC_ALL=C grep -F 'not a git repository' "$pg_tmp/probe-error" > /dev/null ||
        pg_fail 'sot.publishList repository check failed'
    probe=$parent
    while :; do
        [ -r "$probe" ] && [ -x "$probe" ] || pg_fail 'sot.publishList parent is unreadable'
        [ ! -e "$probe/.git" ] && [ ! -L "$probe/.git" ] || pg_fail 'sot.publishList is inside a repository'
        # Also catch the metadata of bare repositories, including nonstandard names.
        if [ -f "$probe/HEAD" ] && [ -d "$probe/objects" ] && [ -f "$probe/config" ]; then
            pg_fail 'sot.publishList is inside Git metadata'
        fi
        [ "$probe" != / ] || break
        probe=${probe%/*}; [ -n "$probe" ] || probe=/
    done
    # Suppress both the utility's and the shell's diagnostics: either can repeat the private path.
    { cat "$value" > "$pg_tmp/list"; } 2>/dev/null || pg_fail 'cannot read sot.publishList'
    pg_list_encoding || pg_fail 'sot.publishList encoding validation failed'
}
