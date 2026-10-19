#!/usr/bin/env bash
# installer-julia-floor.sh -- install.sh's julia_floor_check: the Julia the daemon runs (juliaup's default channel read from
# juliaup.json, else the PATH julia) and the one the comm refresh runs (the PATH julia) are at or above the kernel's compat
# floor, or the run ends ONE STEP LEFT with one line naming the version and the command that fixes it. Sourced by
# installer-wrapper.sh (its check helpers and sandboxed PATH), never run. Juliaup state lives in a scratch JULIAUP_DEPOT_PATH.

# A PATH julia that answers a version query with STUB_VER (and fails when STUB_VER is "none").
mk_julia_floor() {  # <dir>
    mkdir -p "$1"
    cat > "$1/julia" <<'JL'
#!/bin/sh
[ "${STUB_VER:-none}" != none ] || exit 1
case "$*" in *VERSION*) printf '%s' "$STUB_VER" ;; esac
JL
    chmod +x "$1/julia"
    printf '#!/bin/sh\nexit 0\n' > "$1/juliaup"; chmod +x "$1/juliaup"
}

# A juliaup.json whose default channel is at <version> (juliaup's own shape).
mk_juliaup_json() {  # <depot> <version>
    mkdir -p "$1/juliaup"
    printf '{"Default":"release","InstalledChannels":{"release":{"Version":"%s+0.x64.linux.gnu"}},"InstalledVersions":{}}\n' "$2" > "$1/juliaup/juliaup.json"
}

jf="$WORK/julia-floor"; mkdir -p "$jf"
mk_julia_floor "$jf/with-up"
mkdir -p "$jf/no-up"; cp "$jf/with-up/julia" "$jf/no-up/julia"
mk_tools "$jf/tools" jq
src="$(dirname "$0")/../install.sh"
FLOOR_CHECKOUT="$(cd "$(dirname "$0")/../.." && pwd)"

# <bin dir> <default version or "-"> <PATH julia version> [add failed 0|1]: the step lines the check leaves.
floor_run() {
    local depot="$jf/depot-$RANDOM"
    mkdir -p "$depot"
    if [ "$2" != - ]; then mk_juliaup_json "$depot" "$2"; fi
    JULIAUP_DEPOT_PATH="$depot" STUB_VER="$3" PATH="$1:$jf/tools" bash -c 'SOT_INSTALL_SOURCE_ONLY=1; . "$1"; CHECKOUT="$2"; chan=1.12; juliaup_add_failed="$3"; julia_floor_check; printf "%s" "$STEPS_LEFT"' _ "$src" "$FLOOR_CHECKOUT" "${4:-0}" 2>&1
}

case_start "the floor is read from the kernel's compat"
check "the floor" 1.12 "$(PATH="$TOOLS" bash -c 'SOT_INSTALL_SOURCE_ONLY=1; . "$1"; CHECKOUT="$2"; julia_floor' _ "$src" "$FLOOR_CHECKOUT")"

case_start "a default at or above the floor, and a PATH julia at or above it: nothing is left"
check "default at the floor" "" "$(floor_run "$jf/with-up" 1.12.0 1.12.0)"
check "default above the floor" "" "$(floor_run "$jf/with-up" 1.13.2 1.13.2)"

case_start "a default below the floor ends ONE STEP LEFT naming the version and juliaup default"
out="$(floor_run "$jf/with-up" 1.11.7 1.11.7)"
check "names the version the daemon runs" 1 "$(printf '%s' "$out" | grep -c 'daemon runs Julia 1.11.7')"
check "names the fix" 1 "$(printf '%s' "$out" | grep -c 'juliaup default 1.12')"
check "one line" 1 "$(printf '%s\n' "$out" | grep -c .)"

case_start "a default below the floor with a PATH julia above it still ends the step"
out="$(floor_run "$jf/with-up" 1.11.7 1.12.1)"
check "names the daemon's julia" 1 "$(printf '%s' "$out" | grep -c 'daemon runs Julia 1.11.7')"

case_start "no juliaup: the PATH julia is the daemon's and the refresh's"
check "PATH julia above the floor" "" "$(floor_run "$jf/no-up" - 1.12.4)"
out="$(floor_run "$jf/no-up" - 1.10.5)"
check "PATH julia below the floor names the version" 1 "$(printf '%s' "$out" | grep -c 'Julia 1.10.5')"
check "names installing Julia 1.12 or later" 1 "$(printf '%s' "$out" | grep -c 'install Julia 1.12 or later')"

case_start "a PATH julia that cannot be asked ends the step"
out="$(floor_run "$jf/with-up" 1.12.0 none)"
check "says it could not be asked" 1 "$(printf '%s' "$out" | grep -c 'could not be asked')"

case_start "juliaup add failed (offline) and the default is below the floor"
out="$(floor_run "$jf/with-up" 1.11.7 1.11.7 1)"
check "names add and default" 1 "$(printf '%s' "$out" | grep -c 'juliaup add 1.12 && juliaup default 1.12')"
