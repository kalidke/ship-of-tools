#!/usr/bin/env bash
# The scope of a gate run: which pieces of the tree a change touched, from the paths that differ from a named base.
# Sourced by rc-gate.sh and user-manager-tests.sh, which share this one rule (scripts/CLAUDE.md, "The local gate").
#
# A piece is `rust <crate dir under rust/>`, `julia <root | a package path of rc-gate's JULIA_PKGS>`, `shell <comm |
# agents | scripts>` or `docs docs`. A path no piece owns, and every path the gate itself runs on or is built from (the
# workspace manifest and lock, the clippy configs, the gate's own scripts, the publish guard, CI files), is the line
# `whole <path>`: the run widens to everything, never to less. A scope with no differing path is also whole. Only test
# jobs narrow; the lint and the workspace build run in every scope.

# gate_scope_proved_by <rust path>: the shell and Julia pieces whose suites drive that code through the `sotd` binary, one
# line each, on top of the crate's own. The suites run `sotd comm`, `sotd install`, `sotd update`, `sotd topology`,
# `sotd stdio-bridge` and `sotd agent-exec`; the daemon's entry and shared command files are driven by all of them.
gate_scope_proved_by() {
    case $1 in
        rust/backend/src/comm/*|rust/backend/src/rows/*) echo "shell comm"; echo "julia root" ;;
        rust/backend/src/agents/*) echo "shell agents" ;;
        rust/backend/src/install/*|rust/backend/src/update/*|rust/backend/src/update.rs|rust/backend/src/update_tests.rs|rust/backend/src/topology/*|rust/updater/*|rust/topology/*)
            echo "shell scripts"; echo "julia root" ;;
        rust/backend/src/main.rs|rust/backend/src/main_help_tests.rs|rust/backend/src/clients*.rs|rust/backend/src/session.rs|rust/backend/src/paths*.rs|rust/backend/Cargo.toml|rust/backend/build.rs)
            echo "shell comm"; echo "shell agents"; echo "shell scripts"; echo "julia root" ;;
    esac
}

# gate_scope_piece <checkout> <path>: the piece lines of a path (one, or more where a suite drives the code)
gate_scope_piece() {
    local d=$1 p=$2 c
    case $p in
        rust/*/*)
            c=${p#rust/}; c=${c%%/*}
            if [ -f "$d/rust/$c/Cargo.toml" ]; then echo "rust $c"; else echo "whole $p"; return; fi
            gate_scope_proved_by "$p" ;;
        rust/*) echo "whole $p" ;;
        comm/*) echo "shell comm" ;;
        agents/*) echo "shell agents" ;;
        scripts/tests/test-rc-gate-*|scripts/tests/test-gate-*) echo "whole $p" ;;
        scripts/tests/test-*|scripts/tests/installer-*) echo "shell scripts" ;;
        scripts/tests/*|scripts/setup-publish-guard.sh|scripts/hooks/*|scripts/lib/*) echo "whole $p" ;;
        scripts/*) echo "shell scripts" ;;
        .github/*|.claude/*) echo "whole $p" ;;
        docs/*|*.md|LICENSE|logo*.png|logo.ico) echo "docs docs" ;;
        core/*) echo "julia core" ;;
        julia/kernel/*) echo "julia julia/kernel" ;;
        julia/repl/*) echo "julia julia/repl" ;;
        julia/plugins/pdf-file/*) echo "julia julia/plugins/pdf-file" ;;
        julia/plugins/video-file/*) echo "julia julia/plugins/video-file" ;;
        julia/plugins/*/*|src/*|test/*|Project.toml|Manifest.toml) echo "julia root" ;;
        *) echo "whole $p" ;;
    esac
}

# gate_scope <checkout> <base>: the scope of the paths that differ from base (a commit-ish) in the checkout's HEAD, one
# piece line each, sorted, or the single line `whole <path>`. Renames count as both paths. Fails when git does.
gate_scope() {
    local d=$1 base=$2 paths p line pieces=
    paths=$(git -C "$d" diff --name-only --no-renames "$base" HEAD) || return 1
    while IFS= read -r p; do
        [ -n "$p" ] || continue
        line=$(gate_scope_piece "$d" "$p")
        case $line in whole\ *) echo "$line"; return 0 ;; esac
        pieces+=$line$'\n'
    done <<< "$paths"
    if [ -z "$pieces" ]; then echo "whole (no path differs from the base)"; return 0; fi
    printf '%s' "$pieces" | sort -u
}

# The run's scope as the jobs read it: RCG_SCOPE_BASE (the resolved base; empty when the run is not scoped) and
# RCG_SCOPE_SEL (the piece lines). A scope that widened still names its base, and its run is still a scoped run.
# narrow: the run is cut to pieces (scoped, and not widened to whole)
narrow() {
    [ -n "${RCG_SCOPE_BASE:-}" ] && ! grep -q '^whole ' <<< "${RCG_SCOPE_SEL:-}"
}
# want <kind> <name>: the scope selects that piece (always when the run is not narrowed)
want() {
    narrow || return 0
    grep -qxF -- "$1 $2" <<< "$RCG_SCOPE_SEL"
}
# want_kind <kind>: the scope selects some piece of that kind
want_kind() {
    narrow || return 0
    grep -q -- "^$1 " <<< "$RCG_SCOPE_SEL"
}

# gate_scope_ub <checkout> <base>: sets RCG_UB_PARSE (1 when any file under comm/, agents/ or scripts/ differs from base,
# markdown included: the bare in-place sed scan reads every tracked file there) and RCG_UB_SUITES (1 when one other than
# markdown differs) for the under-bash jobs
gate_scope_ub() {
    RCG_UB_PARSE=0 RCG_UB_SUITES=0
    git -C "$1" diff --quiet "$2" HEAD -- comm agents scripts || RCG_UB_PARSE=1
    git -C "$1" diff --quiet "$2" HEAD -- comm agents scripts ':(exclude)*.md' || RCG_UB_SUITES=1
}

# gate_ub_decide <checkout> <line>: sets RCG_UB_PARSE and RCG_UB_SUITES for the under-bash jobs, and SKIPPED when neither
# runs. Both run unless the tree equals the line under comm, agents and scripts (a whole run given a line), or the run is
# narrow and gate_scope_ub finds nothing differing from the scope's base.
gate_ub_decide() {
    RCG_UB_PARSE=1 RCG_UB_SUITES=1 SKIPPED=
    if [ -n "$2" ] && git -C "$1" diff --quiet "$2" HEAD -- scripts comm agents; then
        RCG_UB_PARSE=0 RCG_UB_SUITES=0
        SKIPPED="under-bash skipped: scripts, comm and agents equal $2"
    elif narrow; then
        gate_scope_ub "$1" "$RCG_SCOPE_BASE"
        [ "$RCG_UB_PARSE$RCG_UB_SUITES" != 00 ] ||
            SKIPPED="under-bash skipped: no file under scripts, comm or agents differs from $RCG_SCOPE_BASE"
    fi
}

# gate_scope_resolve <checkout> <who>: reads RCG_SCOPE (the base commit-ish a caller asks for) and sets and exports
# RCG_SCOPE_BASE and RCG_SCOPE_SEL, both empty when no scope was asked for. A scope is refused (status 2, the reason on
# standard error) beside RCG_FULL, for a base that is no commit, and when git cannot list the differing paths.
gate_scope_resolve() {
    RCG_SCOPE_BASE= RCG_SCOPE_SEL=
    if [ -n "${RCG_SCOPE:-}" ]; then
        [ -z "${RCG_FULL:-}" ] || { echo "$2: RCG_SCOPE and RCG_FULL together: a full run is a whole run" >&2; return 2; }
        RCG_SCOPE_BASE=$(git -C "$1" rev-parse --verify -q "$RCG_SCOPE^{commit}") ||
            { echo "$2: RCG_SCOPE $RCG_SCOPE is no commit in $1" >&2; RCG_SCOPE_BASE=; return 2; }
        RCG_SCOPE_SEL=$(gate_scope "$1" "$RCG_SCOPE_BASE") ||
            { echo "$2: cannot read the paths that differ from $RCG_SCOPE" >&2; RCG_SCOPE_BASE= RCG_SCOPE_SEL=; return 2; }
    fi
    export RCG_SCOPE_BASE RCG_SCOPE_SEL
}

# gate_scope_summary: the lines that make a scoped run name itself (none for a whole run)
gate_scope_summary() {
    [ -n "${RCG_SCOPE_BASE:-}" ] || return 0
    echo "scoped base $RCG_SCOPE_BASE"
    sed 's/^/scope /' <<< "$RCG_SCOPE_SEL"
}

# gate_verdict <failed 0|1>: a summary's last line. A scoped run that passed is ALLDONE SCOPED, never ALLDONE: only a
# whole run passes where a release asks for ALLDONE.
gate_verdict() {
    if [ "$1" -ne 0 ]; then echo "ALLDONE FAILED"
    elif [ -n "${RCG_SCOPE_BASE:-}" ]; then echo "ALLDONE SCOPED"
    else echo ALLDONE
    fi
}
