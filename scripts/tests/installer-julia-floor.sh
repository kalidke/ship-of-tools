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

# A juliaup.json whose default channel is at <version> (juliaup's own shape); FR_HAS=1 also lists an installed 1.12 channel.
mk_juliaup_json() {  # <depot> <version>
    local extra=""
    [ "${FR_HAS:-0}" != 1 ] || extra=',"1.12":{"Version":"1.12.7+0.x64.linux.gnu"}'
    mkdir -p "$1/juliaup"
    printf '{"Default":"release","InstalledChannels":{"release":{"Version":"%s+0.x64.linux.gnu"}%s},"InstalledVersions":{}}\n' "$2" "$extra" > "$1/juliaup/juliaup.json"
}

jf="$WORK/julia-floor"; mkdir -p "$jf"
mk_julia_floor "$jf/with-up"
mkdir -p "$jf/no-up"; cp "$jf/with-up/julia" "$jf/no-up/julia"
mk_tools "$jf/tools" jq
src="$(dirname "$0")/../install.sh"
FLOOR_CHECKOUT="$(cd "$(dirname "$0")/../.." && pwd)"

# <bin dir> <default version or "-"> <PATH julia version>: the lines the check leaves (a stop prints its FAIL line instead).
# FR_SEL=1: a +1.12 channel was selected for the steps; FR_HAS=1: juliaup.json lists a 1.12 channel; SOT_JULIA_BIN passes through.
floor_run() {
    local depot="$jf/depot-$RANDOM"
    mkdir -p "$depot"
    if [ "$2" != - ]; then mk_juliaup_json "$depot" "$2"; fi
    JULIAUP_DEPOT_PATH="$depot" STUB_VER="$3" PATH="$1:$jf/tools" bash -c 'SOT_INSTALL_SOURCE_ONLY=1; . "$1"; CHECKOUT="$2"; chan=1.12; JULIA_SEL=(); [ "${FR_SEL:-0}" != 1 ] || JULIA_SEL=(+1.12); julia_floor_check; printf "%s" "$STEPS_LEFT"' _ "$src" "$FLOOR_CHECKOUT" 2>&1 || true
}

case_start "the floor is read from the kernel's compat"
check "the floor" 1.12 "$(PATH="$TOOLS" bash -c 'SOT_INSTALL_SOURCE_ONLY=1; . "$1"; CHECKOUT="$2"; julia_floor' _ "$src" "$FLOOR_CHECKOUT")"

case_start "the floor is the first major.minor of the compat, whatever its spelling"
mkdir -p "$jf/spell/julia/kernel"
for spelling in 'julia = "1.12.4"' 'julia = "~1.12"' 'julia  =  "^1.12, 1.13"' 'julia = ">= 1.12"'; do
    printf '[compat]\n%s\n' "$spelling" > "$jf/spell/julia/kernel/Project.toml"
    check "$spelling" 1.12 "$(PATH="$TOOLS" bash -c 'SOT_INSTALL_SOURCE_ONLY=1; . "$1"; CHECKOUT="$2"; julia_floor' _ "$src" "$jf/spell")"
done

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

case_start "a default below the floor: the fix keys on whether the 1.12 channel is installed"
out="$(floor_run "$jf/with-up" 1.11.7 1.11.7)"
check "not installed: add, then default" 1 "$(printf '%s' "$out" | grep -c 'juliaup add 1.12 && juliaup default 1.12')"
out="$(FR_HAS=1 floor_run "$jf/with-up" 1.11.7 1.11.7)"
check "installed: default alone" 1 "$(printf '%s' "$out" | grep -c 'run  juliaup default 1.12$')"

case_start "a juliaup.json default below the floor with no juliaup on PATH says to install Julia"
out="$(floor_run "$jf/no-up" 1.11.7 1.11.7)"
check "names installing Julia 1.12 or later" 1 "$(printf '%s' "$out" | grep -c 'install Julia 1.12 or later')"

case_start "the PATH julia below the floor with no channel selected stops the run before any step"
out="$(floor_run "$jf/with-up" 1.11.7 1.11.7)"
check "a FAIL line naming the version and the command" 1 "$(printf '%s' "$out" | grep -c '^FAIL: .*Julia 1.11.7.*juliaup')"
out="$(floor_run "$jf/no-up" - 1.10.5)"
check "no juliaup: a FAIL line too" 1 "$(printf '%s' "$out" | grep -c '^FAIL: .*Julia 1.10.5')"

case_start "a default at the floor and an askable PATH julia below it, with 1.12 selected: the refresh's step"
out="$(FR_SEL=1 floor_run "$jf/with-up" 1.12.0 1.11.7)"
check "names the refresh's version" 1 "$(printf '%s' "$out" | grep -c 'comm refresh runs Julia 1.11.7')"
check "does not name the daemon" 0 "$(printf '%s' "$out" | grep -c 'daemon runs')"
check "tells to put a newer Julia ahead of the PATH one" 1 "$(printf '%s' "$out" | grep -c 'ahead of .*julia')"
check "no juliaup default (it would not change the PATH julia)" 0 "$(printf '%s' "$out" | grep -c 'juliaup default')"

case_start "SOT_JULIA_BIN is the daemon's Julia, as julia.rs reads it first"
for v in old:1.11.7 new:1.12.4; do
    mkdir -p "$jf/bin-${v%%:*}"
    printf '#!/bin/sh\nprintf %%s %s\n' "${v#*:}" > "$jf/bin-${v%%:*}/julia"; chmod +x "$jf/bin-${v%%:*}/julia"
done
out="$(SOT_JULIA_BIN="$jf/bin-old/julia" FR_SEL=1 floor_run "$jf/with-up" 1.12.0 1.12.0)"
check "a binary below the floor ends the step naming it" 1 "$(printf '%s' "$out" | grep -c 'daemon runs Julia 1.11.7')"
check "and does not offer juliaup" 0 "$(printf '%s' "$out" | grep -c 'juliaup default')"
check "and names SOT_JULIA_BIN" 1 "$(printf '%s' "$out" | grep -c 'SOT_JULIA_BIN')"
out="$(SOT_JULIA_BIN=julia FR_SEL=1 floor_run "$jf/with-up" 1.12.0 1.12.0)"
check "a relative value is refused" 1 "$(printf '%s' "$out" | grep -c 'not an absolute path')"
out="$(SOT_JULIA_BIN="$jf/gone/julia" FR_SEL=1 floor_run "$jf/with-up" 1.12.0 1.12.0)"
check "a removed binary cannot be asked" 1 "$(printf '%s' "$out" | grep -c 'could not be asked')"
out="$(SOT_JULIA_BIN="$jf/bin-new/julia" FR_SEL=1 floor_run "$jf/with-up" 1.11.7 1.12.0)"
check "an override at the floor beats a default below it" "" "$out"

# Each printed command, run against a copy of the real juliaup and a scratch state (its own HOME and JULIAUP_DEPOT_PATH; the
# box's juliaup is never touched), clears the step. Needs a juliaup binary and its julia launcher; `juliaup add` needs the network.
real_up="$(command -v juliaup || true)"; [ -x "$real_up" ] || real_up="$HOME/.juliaup/bin/juliaup"
real_jl="$(dirname "$real_up")/julia"
# <name> <has 1.12: 0|1>: a juliaup whose default channel is lts at 1.10.11; sets JU (the dir) and runs nothing.
mk_scratch_juliaup() {
    JU="$jf/ju-$1"; mkdir -p "$JU/bin" "$JU/depot/juliaup" "$JU/home"
    cp "$real_up" "$real_jl" "$JU/bin/"
    local v ch='"lts":{"Version":"1.10.11+0.x64.linux.gnu"}' iv='"1.10.11+0.x64.linux.gnu":{"Path":"./julia-1.10.11"}'
    if [ "$2" = 1 ]; then ch="$ch"',"1.12":{"Version":"1.12.7+0.x64.linux.gnu"}'; iv="$iv"',"1.12.7+0.x64.linux.gnu":{"Path":"./julia-1.12.7"}'; fi
    for v in 1.10.11 1.12.7; do
        mkdir -p "$JU/depot/juliaup/julia-$v/bin"
        printf '#!/bin/sh\nprintf %%s %s\n' "$v" > "$JU/depot/juliaup/julia-$v/bin/julia"; chmod +x "$JU/depot/juliaup/julia-$v/bin/julia"
    done
    printf '{"Version":"1.1.0","InstalledVersions":{%s},"InstalledChannels":{%s},"Default":"lts","Overrides":[],"Settings":{"create_channel_symlinks":false,"versionsdb_update_interval":1440},"LastVersionDbUpdateUnixTimeSeconds":0}\n' "$iv" "$ch" > "$JU/depot/juliaup/juliaup.json"
}
ju_env() { env -i HOME="$JU/home" PATH="$JU/bin:$jf/tools" JULIAUP_DEPOT_PATH="$JU/depot" "$@"; }
ju_check() { ju_env bash -c 'SOT_INSTALL_SOURCE_ONLY=1; . "$1"; CHECKOUT="$2"; chan=1.12; JULIA_SEL=(); julia_floor_check; printf "%s" "$STEPS_LEFT"' _ "$src" "$FLOOR_CHECKOUT" 2>&1 || true; }

case_start "the printed command, run against a scratch juliaup, clears the step"
if [ -x "$real_up" ] && [ -x "$real_jl" ]; then
    for has in 1 0; do
        mk_scratch_juliaup "has$has" "$has"
        out="$(ju_check)"
        cmd="${out##*run  }"
        check "has12=$has: the check stops on the lts default" 1 "$(printf '%s' "$out" | grep -c '^FAIL: .*Julia 1.10.11')"
        if [ "$has" = 1 ]; then
            check "has12=1: the command is juliaup default alone" "juliaup default 1.12" "$cmd"
        else
            check "has12=0: the command adds, then sets the default" "juliaup add 1.12 && juliaup default 1.12" "$cmd"
        fi
        if ju_env timeout 60 bash -c "$cmd" >/dev/null 2>&1; then
            check "has12=$has: the same check after the command leaves nothing" "" "$(ju_check)"
        elif [ "$has" = 0 ]; then
            printf '    SKIP has12=0: juliaup add needs the network, and it failed\n'
        else
            check "has12=$has: the command ran" 0 1
        fi
    done
    mk_scratch_juliaup none 0
    check "juliaup default 1.12 alone fails without the channel (why add comes first)" 1 "$(ju_env juliaup default 1.12 >/dev/null 2>&1 && echo 0 || echo 1)"
else
    printf '    SKIP no juliaup binary on this box\n'
fi
