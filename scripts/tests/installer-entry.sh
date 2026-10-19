#!/usr/bin/env bash
# installer-entry.sh -- "update sot from the repo" on Unix: install.sh run end to end, as the entry of an update. Sourced
# by installer-state.sh (its check helpers and sandboxed tools), never run. The target it follows from install.json, the
# direction it refuses to go, the role it recovers, the one verdict it ends with, the daemon it asks about before it
# restarts one, the dirty old folder it keeps, the launcher that dials the topology plan, and a hub that omits this box.
#
# install.sh runs for real, from this tree, in a scratch HOME. The release is a fixture: a git origin the base clone
# fetches from (a path, never GitHub), an archive of stand-in binaries, and `curl`, `git`-free stubs that serve it.
# `sotd`, `systemctl`, `julia` and `ps` are stand-ins driven by files in $CTL, so each case sets the facts it needs
# (the topology, the daemon's version, the live rows) and reads the calls the run made. The real sotd's two answers
# (`update direction`, `update repls`) are pinned in rust/backend/src/update/cli.rs and, with SOT_TEST_SOTD, below.

if [ "$(uname -s)" != Linux ]; then
    printf '  SKIP the entry cases (they run install.sh with a systemd unit: Linux only)\n'
    return 0
fi

ENTRY_SRC="$(cd "$(dirname "$0")/../.." && pwd)"
ENTRY_TOOLS="$WORK/entry-tools"; mk_tools "$ENTRY_TOOLS" tar gzip tr jq ldd sort tail
NOTTY="$(command -v setsid || true)"

# A fixture: <d>/home (HOME), <d>/origin (the git origin, two tags), <d>/rel (the v9.9.9 archive folder and the files
# the curl stand-in serves) and <d>/ctl (the stand-ins' facts). The base clone is in place, so no run reaches GitHub.
entry_fixture() {  # <d>
    local d="$1" tgt="$TGT" top
    top="sot-9.9.9-$tgt"
    mkdir -p "$d/home/.config/systemd/user" "$d/home/.local/share/sot/repo" "$d/ctl" "$d/stubs" "$d/origin/scripts/lib" \
        "$d/origin/deploy" "$d/origin/julia/kernel" "$d/origin/julia/repl" "$d/origin/julia/pluto" "$d/rel/$top" "$d/comm"
    cp "$ENTRY_SRC/scripts/lib/sot-daemon.sh" "$ENTRY_SRC/scripts/lib/sot-hosts.sh" "$d/origin/scripts/lib/"
    cp "$ENTRY_SRC/scripts/launch-sot.sh" "$ENTRY_SRC/scripts/sot-apply.sh" "$d/origin/scripts/"
    cp "$ENTRY_SRC/deploy/sotd.service" "$d/origin/deploy/"
    for e in kernel repl pluto; do printf 'name = "%s"\n' "$e" > "$d/origin/julia/$e/Project.toml"; done
    printf 'Manifest.toml\n' > "$d/origin/.gitignore"
    printf 'logo\n' > "$d/origin/logo.png"
    (
        cd "$d/origin" && git init -q . && git add . \
            && git -c user.name=t -c user.email=t@t commit -q -m one && git tag v9.9.8 \
            && printf 'two\n' > two.txt && git add . && git -c user.name=t -c user.email=t@t commit -q -m two && git tag v9.9.9
    )
    git clone -q --no-checkout "$d/origin" "$d/home/.local/share/sot/repo/base"
    git -C "$d/origin" rev-parse HEAD > "$d/ctl/commit"
    cut -c1-9 "$d/ctl/commit" > "$d/ctl/short"
    cat > "$d/stubs/sotd" <<'SD'
#!/bin/bash
# The facts are files in $CTL; every call is logged with its argv.
echo "sotd $*" >> "$CTL/sotd.log"
f() { cat "$CTL/$1" 2>/dev/null; }
rc() { local v; v="$(f "$1")"; exit "${v:-$2}"; }
case "$1 $2" in
    "--version "*) echo "sotd 9.9.9 ($(f short) 2026-10-17, protocol 3)"; exit 0 ;;
    "topology status")
        if [ -f "$CTL/topo-status" ]; then printf 'HOST DECLARED\n'; f topo-status; exit 0; fi
        echo "sotd topology: no hosts.toml at /x/hosts.toml (run sotd topology sync --hub ALIAS)" >&2; exit 2 ;;
    "topology plan")
        if [ -f "$CTL/plan" ]; then f plan; exit 0; fi
        echo "sotd topology: no hosts.toml" >&2; exit 2 ;;
    "topology sync") f sync-out >&2; rc sync-rc 0 ;;
    "topology pin") exit 0 ;;
    "trust declare") echo Declared; exit 0 ;;
    "session-socket-path sot"|"session-socket-path ") echo "$CTL/sot.sock"; exit 0 ;;
    "update direction") if [ -f "$CTL/direction" ]; then f direction; exit 0; fi; exit 1 ;;
    "update repls") f repls-out; f repls-err >&2; rc repls-rc 0 ;;
    "same-version --endpoint") f same-out; f same-err >&2; rc same-rc 2 ;;
    "stdio-bridge --endpoint") exit 0 ;;
esac
exit 0
SD
    printf '#!/bin/sh\nif [ "$1" = --version ]; then echo "sot 9.9.9 ($(cat "$CTL/short") 2026-10-17, protocol 3)"; exit 0; fi\necho "sot $*" >> "$CTL/sot.log"\n' > "$d/rel/$top/sot"
    printf '#!/bin/sh\nexit 0\n' > "$d/rel/$top/sot-capsule"
    cp "$ENTRY_SRC/scripts/sot-apply.sh" "$d/rel/$top/sot-apply"
    cp "$d/stubs/sotd" "$d/rel/$top/sotd"
    cp "$ENTRY_SRC/deploy/sotd.service" "$d/rel/$top/sotd.service"
    chmod +x "$d/rel/$top"/*
    tar -czf "$d/rel/$top.tar.gz" -C "$d/rel" "$top"
    cp "$d/ctl/commit" "$d/rel/COMMIT"
    ( cd "$d/rel" && sha256_of "$top.tar.gz" COMMIT > SHA256SUMS )
    cat > "$d/stubs/curl" <<'CU'
#!/bin/sh
echo "curl $*" >> "$CTL/net.log"
out=""; url=""
while [ $# -gt 0 ]; do case "$1" in -o) out="$2"; shift ;; http*) url="$1" ;; esac; shift; done
case "$url" in
    */releases/download/*) cp "$CTL/../rel/${url##*/}" "$out" ;;
    */releases/latest) printf '%s' "https://github.com/x/releases/tag/$(cat "$CTL/latest")" ;;
    *api.github.com*releases*) cat "$CTL/api.json" ;;
    */scripts/lib/sot-daemon.sh) ;;
    */scripts/install.sh) printf '#!/bin/sh\necho "tag=$SOT_INSTALL_TAG implicit=${SOT_INSTALL_IMPLICIT:-} args=$*" >> "$CTL/prelude.log"\n' > "$out" ;;
    *) exit 7 ;;
esac
CU
    cat > "$d/stubs/systemctl" <<'SC'
#!/bin/sh
echo "systemctl $*" >> "$CTL/systemctl.log"
case "$*" in
    "--user is-active"*) [ -f "$CTL/unit-active" ]; exit $? ;;
    "--user cat sotd.service") [ -f "$CTL/unit-active" ] && printf "ExecStart=/bin/bash -c 'exec \"%s/bin/sotd\"'\n" "$HOME/.local/share/sot"; exit 0 ;;
    *"enable --now"*) [ -f "$CTL/pin-excludes" ] || : > "$CTL/unit-active"; [ ! -f "$CTL/start-same" ] || echo 0 > "$CTL/same-rc" ;;
    *try-restart*) [ ! -f "$CTL/restart-same" ] || echo 0 > "$CTL/same-rc" ;;
esac
exit 0
SC
    cat > "$d/stubs/julia" <<'JU'
#!/bin/sh
echo "julia $*" >> "$CTL/julia.log"
case "$*" in *update_comm*)
    n="$(cat "$CTL/julia-fail" 2>/dev/null || echo 0)"
    if [ "$n" -gt 0 ]; then echo $((n - 1)) > "$CTL/julia-fail"; exit 1; fi
    mkdir -p "$SOT_COMM_HOME/bin"
    for a in "$@"; do case "$a" in --project=*) p="${a#--project=}" ;; esac; done
    git -C "$p" rev-parse --short=9 HEAD > "$SOT_COMM_HOME/VERSION"
    [ ! -f "$CTL/hook" ] || . "$CTL/hook" ;;
*Pkg.instantiate*) for a in "$@"; do case "$a" in --project=*) : > "${a#--project=}/Manifest.toml" ;; esac; done ;;
esac
exit 0
JU
    printf '#!/bin/sh\necho "ldd (stand-in) 2.35"\n' > "$d/stubs/ldd"
    printf '#!/bin/sh\ncat "$CTL/ps" 2>/dev/null\n' > "$d/stubs/ps"
    chmod +x "$d/stubs"/*
    printf 'v9.9.9' > "$d/ctl/latest"
}

# entry_run <d> <install.sh args...>: the installer as an agent runs it, sets ENTRY_OUT and ENTRY_RC. TMPDIR is scratch,
# the PATH is the stand-ins and the library's tools, and the environment is nothing else. A run with no terminal cannot
# ask a question. Names that start ENTRY_ENV are passed through (e.g. SOT_INSTALL_TAG=v9.9.9 SOT_INSTALL_IMPLICIT=1).
entry_run() {
    local d="$1"; shift
    local -a pass=()
    while [ $# -gt 0 ] && [[ "$1" == SOT_* ]]; do pass+=("$1"); shift; done
    ENTRY_RC=0
    ENTRY_OUT="$(env -i HOME="$d/home" PATH="$d/stubs:$ENTRY_TOOLS" CTL="$d/ctl" SOT_COMM_HOME="$d/comm" SOT_SELF_HOST=entrybox \
        TMPDIR="$d" ${pass[@]+"${pass[@]}"} ${NOTTY:+"$NOTTY" -w} bash "$ENTRY_SRC/scripts/install.sh" "$@" 2>&1)" || ENTRY_RC=$?
    ENTRY_LAST="$(printf '%s\n' "$ENTRY_OUT" | tail -1)"
}
entry_archive() {  # <d> [SOT_*=...] <install.sh args...>
    local d="$1" pre=(); shift
    while [[ "${1:-}" == SOT_* ]]; do pre+=("$1"); shift; done
    entry_run "$d" ${pre[@]+"${pre[@]}"} --archive "$d/rel" "$@"
}
sotd_log() { cat "$1/ctl/sotd.log" 2>/dev/null || true; }
count_in() { local n=0; [ ! -f "$1" ] || n="$(grep -c -- "$2" "$1" || true)"; printf '%s' "$n"; }
said() { printf '%s\n' "$ENTRY_OUT" | grep -c -F -- "$1" || true; }
entry_installed() { sed -n 's/^ *"version": *"\([^"]*\)".*/\1/p' "$1/home/.local/share/sot/install.json" 2>/dev/null | head -1; }
entry_prefix_tree() { tree_of "$1/home/.local" "$1/comm"; }
# A recorded install: the install.json an earlier run left, at the fixture's prefix.
entry_record() {  # <d> <version> <daemon 0|1> <frontend 0|1> <service> [hub]
    local P="$1/home/.local/share/sot"
    mkdir -p "$P"
    installer_manifest_json "$P" "$1/home/.config/sot" "$5" "$2" "v$2" 0000 2026-10-01T00:00:00Z "${6:-}" "$3" "$4" > "$P/install.json"
}

# ---------------------------------------------------------------------------
case_start "entry_takes_the_installed_channel"
# The prelude, with no --version: no install and a stable one take GitHub's latest stable; a pre-release takes the newest
# published release of any kind; a build with a + stops before any asset is fetched.
d="$WORK/ch"; entry_fixture "$d"
printf '[\n  {\n    "tag_name": "v9.9.10-rc1",\n    "assets": [ { "name": "x" } ]\n  },\n  {\n    "tag_name": "v9.9.9"\n  }\n]\n' > "$d/ctl/api.json"
prelude() { : > "$d/ctl/net.log"; : > "$d/ctl/prelude.log"; entry_run "$d" "$@"; }
prelude --be-only
check "no install: the latest stable, an implicit run" "tag=v9.9.9 implicit=1 args=--be-only" "$(cat "$d/ctl/prelude.log")"
check "no install: no release list is read" "0" "$(count_in "$d/ctl/net.log" 'api.github.com')"
entry_record "$d" 9.9.8 1 0 none
prelude --be-only
check "a stable release: the latest stable" "tag=v9.9.9 implicit=1 args=--be-only" "$(cat "$d/ctl/prelude.log")"
entry_record "$d" 0.6.6-rc9.13 1 0 none
prelude --be-only
check "a pre-release: the newest published release, not the latest stable" "tag=v9.9.10-rc1 implicit=1 args=--be-only" "$(cat "$d/ctl/prelude.log")"
check "a pre-release: it asks the release list for its first entry" "1" "$(count_in "$d/ctl/net.log" 'api.github.com/repos/kalidke/ship-of-tools/releases?per_page=1')"
before="$(entry_prefix_tree "$d")"
. "$(dirname "${BASH_SOURCE[0]}")/installer-build-direction.sh"
entry_record "$d" 0.6.6-rc9.13 1 0 none
prelude --version v9.9.1 --be-only
check "--version names the tag, and the run is explicit" "tag=v9.9.1 implicit=0 args=--version v9.9.1 --be-only" "$(cat "$d/ctl/prelude.log")"
check "--version reads no channel" "0" "$(count_in "$d/ctl/net.log" 'api.github.com\|releases/latest')"
mkdir -p "$d/other"; installer_manifest_json "$d/other" "$d/c" none 0.6.6-rc9.13 v0.6.6-rc9.13 0000 2026-10-01T00:00:00Z "" 1 0 > "$d/other/install.json"
entry_record "$d" 9.9.8 1 0 none
prelude --prefix "$d/other" --be-only
check "--prefix says which install.json is read" "tag=v9.9.10-rc1 implicit=1 args=--prefix $d/other --be-only" "$(cat "$d/ctl/prelude.log")"
check "the channel of the installed version is a pure answer" "latest newest stop latest" \
    "$(installer_channel '') $(installer_channel 0.6.6-rc9.13) $(installer_channel 0.6.6-rc9.14-dev+a) $(installer_channel 0.6.5)"
check "a record is read by key" "0.6.6-rc9.13 true false" \
    "$(installer_json_string "$d/other/install.json" version) $(installer_json_bool "$d/other/install.json" daemon) $(installer_json_bool "$d/other/install.json" frontend)"

# ---------------------------------------------------------------------------
case_start "entry_never_moves_back_unless_told"
# The target's own sotd says which way the run moves the install. An implicit run (the prelude says so) that would move
# it back fails before anything under the prefix changes; naming the version proceeds and says the direction.
d="$WORK/dir"; entry_fixture "$d"
entry_record "$d" 9.9.10 1 0 none
before="$(entry_prefix_tree "$d")"
printf 'older\n' > "$d/ctl/direction"
entry_run "$d" SOT_INSTALL_TAG=v9.9.9 SOT_INSTALL_IMPLICIT=1 --be-only --no-service
check "an implicit run that would move back stops (exit 2)" "2" "$ENTRY_RC"
check "it names the installed version and --version" "1" "$(said 'FAIL: direction: this computer runs 9.9.10, which is newer than 9.9.9, the release this run found; it never moves back unless told: name the version you want with --version v9.9.9')"
check "nothing under the prefix, ~/.local or comm changed" "$before" "$(entry_prefix_tree "$d")"
check "the sotd was asked the installed way" "1" "$(sotd_log "$d" | grep -c -x -F "sotd update direction 9.9.9 --prefix $d/home/.local/share/sot" || true)"
entry_archive "$d" --be-only --no-service
check "naming the target (--archive) goes on and says older" "0 1" "$ENTRY_RC $(said 'this run moves the install from 9.9.10 to 9.9.9 (older)')"
check "and ends in PASS, which says the install is pinned" "PASS: Ship of Tools 9.9.9 at $(cat "$d/ctl/short") on this computer; pinned at 9.9.9: automatic updates stay off until a run that names no version" "$ENTRY_LAST"
entry_record "$d" 9.9.10 1 0 none; printf 'older\n' > "$d/ctl/direction"
entry_run "$d" SOT_INSTALL_TAG=v9.9.9 --be-only --no-service
check "a pinned run (no implicit mark) goes on" "0" "$ENTRY_RC"
printf 'same\n' > "$d/ctl/direction"
entry_run "$d" SOT_INSTALL_TAG=v9.9.9 SOT_INSTALL_IMPLICIT=1 --be-only --no-service
check "an implicit run at the installed version is a repair run" "0 1" "$ENTRY_RC $(said 'a repair run')"
printf 'newer\n' > "$d/ctl/direction"
entry_run "$d" SOT_INSTALL_TAG=v9.9.9 SOT_INSTALL_IMPLICIT=1 --be-only --no-service
check "an implicit run that moves forward goes on" "0 1" "$ENTRY_RC $(said '(newer)')"
printf 'nonsense\n' > "$d/ctl/direction"
entry_run "$d" SOT_INSTALL_TAG=v9.9.9 --be-only --no-service
check "a pinned run whose sotd cannot say goes on, and says so" "0 1" "$ENTRY_RC $(said 'could not tell which way')"
entry_record "$d" 9.9.10 1 0 none
entry_run "$d" SOT_INSTALL_TAG=v9.9.9 SOT_INSTALL_IMPLICIT=1 --be-only --no-service
check "an implicit run whose sotd cannot say stops like older (exit 2)" "2" "$ENTRY_RC"
starts_with "and names --version" "FAIL: direction: could not tell which way v9.9.9 moves the install" "$ENTRY_LAST"
check "nothing changed" "yes" "$([ "$(entry_installed "$d")" = 9.9.10 ] && echo yes || echo no)"
d="$WORK/dir-first"; entry_fixture "$d"
entry_run "$d" SOT_INSTALL_TAG=v9.9.9 SOT_INSTALL_IMPLICIT=1 --be-only --no-service
check "a first install asks nothing of the direction" "0 0" "$ENTRY_RC $(sotd_log "$d" | grep -c 'update direction' || true)"

# ---------------------------------------------------------------------------
case_start "entry_keeps_the_recorded_role"
# With no role flag and no topology entry, the install already here says what it was.
d="$WORK/role"; entry_fixture "$d"; P="$d/home/.local/share/sot"
printf '#!/usr/bin/env bash\nexport SOT_HOST="gpu-box"\nexec x\n' > "$d/wrapper"
rec() { installer_recorded_role "$P/install.json" "$d/wrapper"; }
entry_record "$d" 0.6.6-rc9.13 1 1 systemd; check "daemon and frontend: local" "local -" "$(rec)"
entry_record "$d" 0.6.6-rc9.13 1 0 none; check "daemon only: be-only" "be-only -" "$(rec)"
entry_record "$d" 0.6.6-rc9.13 0 1 none; check "frontend only: remote, with the alias of its wrapper" "remote gpu-box" "$(rec)"
printf '#!/bin/sh\nexec x\n' > "$d/wrapper"; check "a remote record whose wrapper names no alias recovers nothing" "" "$(rec)"
printf '{\n  "schema": 1,\n  "role": "be-only",\n  "version": "0.5.10"\n}\n' > "$P/install.json"; check "a 0.5.x role key maps to its flag" "be-only -" "$(rec)"
printf '{\n  "schema": 1,\n  "version": "0.5.10"\n}\n' > "$P/install.json"; check "a record with no role recovers nothing" "" "$(rec)"
check "no record recovers nothing" "" "$(installer_recorded_role "$d/absent.json" "$d/wrapper")"

entry_record "$d" 0.6.6-rc9.13 1 0 none
before="$(entry_prefix_tree "$d")"
entry_archive "$d"
check "be-only with no service, from the record alone: PASS" "0 PASS" "$ENTRY_RC $(printf '%s' "$ENTRY_LAST" | cut -c1-4)"
check "it says what it kept" "1" "$(said 'recorded --be-only --no-service; keeping it')"
check "the record keeps its role bits and writes no unit" "true false none 0" \
    "$(sed -n 's/^ *"daemon": *\([a-z]*\).*/\1/p' "$P/install.json") $(sed -n 's/^ *"frontend": *\([a-z]*\).*/\1/p' "$P/install.json") $(sed -n 's/^ *"service": *"\([a-z]*\)".*/\1/p' "$P/install.json") $(count_in "$d/ctl/systemctl.log" 'enable')"
check "no launcher is written for a daemon-only install" "no" "$([ -e "$d/home/.local/bin/sot-launch" ] && echo yes || echo no)"
entry_record "$d" 0.6.6-rc9.13 1 1 systemd
entry_archive "$d" --no-service
check "a role flag-less run with a local record is all-in-one" "0 yes" "$ENTRY_RC $(grep -q '^# sot-launch: all-in-one$' "$d/home/.local/bin/sot-launch" && echo yes || echo no)"
# Nothing recorded and no way to ask: exit 2, naming the flags.
d="$WORK/role-none"; entry_fixture "$d"
if [ -n "$NOTTY" ]; then
    entry_archive "$d"
    check "no record, no flag, no terminal: exit 2 naming the flags" "2 1" "$ENTRY_RC $(said 'FAIL: role: pick a role: --local | --backend <alias> | --be-only')"
else
    printf '    SKIP no record and no terminal (no setsid here)\n'
fi
# A hub the install recorded is refreshed again, and a hub that cannot be reached does not fail an offline update.
d="$WORK/role-hub"; entry_fixture "$d"
entry_record "$d" 0.6.6-rc9.13 1 0 none myhub; printf '3\n' > "$d/ctl/sync-rc"; printf 'ssh: connect to host myhub: unreachable\n' > "$d/ctl/sync-out"
entry_archive "$d" --no-service
check "the recorded hub is synced again" "yes" "$(sotd_log "$d" | grep -q -x -F 'sotd topology sync --hub myhub' && echo yes || echo no)"
check "an unreachable recorded hub is a note, not a failure" "0 1" "$ENTRY_RC $(said "could not refresh hosts.toml from the recorded hub 'myhub'")"
check "the hub stays recorded" "myhub" "$(sed -n 's/^ *"hub": *"\([^"]*\)".*/\1/p' "$d/home/.local/share/sot/install.json")"

# ---------------------------------------------------------------------------
case_start "entry_ends_with_one_verdict"
check "PASS names the version and the commit" "PASS: Ship of Tools 1.2.3 at abc123def on this computer" "$(installer_verdict 1.2.3 abc123def '' '')"
check "a step left is ONE STEP LEFT, steps joined" "ONE STEP LEFT: quit it; start it" "$(installer_verdict 1.2.3 abc '' 'quit it; start it')"
check "a pinned PASS ends with where it is pinned" "PASS: Ship of Tools 1.2.3 at abc on this computer; pinned at 1.2.3: automatic updates stay off until a run that names no version" "$(installer_verdict 1.2.3 abc '' '' 1.2.3)"
check "a pinned ONE STEP LEFT does too" "ONE STEP LEFT: quit it; pinned at 1.2.3: automatic updates stay off until a run that names no version" "$(installer_verdict 1.2.3 abc '' 'quit it' 1.2.3)"
check "a FAIL after the pin was written ends with it too" "FAIL: comm: stale; pinned at 1.2.3: automatic updates stay off until a run that names no version" "$(installer_verdict 1.2.3 abc 'comm: stale' 'quit it' 1.2.3)"
check "and an unpinned run's FAIL names none" "FAIL: comm: stale" "$(installer_verdict 1.2.3 abc 'comm: stale' '' '')"
check "a failed check is FAIL and wins over a step" "FAIL: comm: stale" "$(installer_verdict 1.2.3 abc 'comm: stale' 'quit it')"
d="$WORK/vd"; entry_fixture "$d"; short="$(cat "$d/ctl/short")"
entry_archive "$d" --be-only --no-service
check "a whole install ends PASS, as its one last line (a pack names its version, so it is pinned)" "PASS: Ship of Tools 9.9.9 at $short on this computer; pinned at 9.9.9: automatic updates stay off until a run that names no version|0" "$ENTRY_LAST|$ENTRY_RC"
check "the verdict is the only line that starts with PASS, FAIL or ONE STEP" "1" "$(printf '%s\n' "$ENTRY_OUT" | grep -c -E '^(PASS|FAIL|ONE STEP LEFT):' || true)"
broken() {  # <description> <expected fail prefix> <hook>: a run whose comm refresh leaves the install broken one way
    local dd="$WORK/vd-$RANDOM"; entry_fixture "$dd"; printf '%s\n' "$3" > "$dd/ctl/hook"
    entry_archive "$dd" --be-only --no-service
    check "$1: exit 1" "1" "$ENTRY_RC"
    starts_with "$1: FAIL names the check" "FAIL: $2" "$ENTRY_LAST"
    check "$1: and, the archive having pinned the install, the pin" "yes" "$(case "$ENTRY_LAST" in (*'; pinned at 9.9.9: automatic updates stay off until a run that names no version') echo yes ;; (*) echo no ;; esac)"
}
broken "a binary that reports another version" "binaries" 'printf "#!/bin/sh\necho sotd 1.0.0\n" > "$HOME/.local/share/sot/bin/sotd"'
broken "an update pointer armed again" "pointer" ': > "$HOME/.local/share/sot/updates/pending-'"$TGT"'.json"'
broken "agent comm at another commit" "comm" 'echo deadbeef0 > "$SOT_COMM_HOME/VERSION"'
d2="$WORK/vd-julia"; entry_fixture "$d2"
cat > "$d2/stubs/julia" <<'JU'
#!/bin/sh
case "$*" in *update_comm*) mkdir -p "$SOT_COMM_HOME/bin"; git -C "${2#--project=}" rev-parse --short=9 HEAD > "$SOT_COMM_HOME/VERSION" ;; esac
exit 0
JU
entry_archive "$d2" --be-only --no-service
check "a Julia environment that was never instantiated: exit 1" "1" "$ENTRY_RC"
starts_with "its FAIL names the environment" "FAIL: julia: julia/kernel has no Manifest.toml" "$ENTRY_LAST"
# The daemon: a window that holds the old one is a step left, an owned unit that did not come back is a failure.
d="$WORK/vd-held"; entry_fixture "$d"; P="$d/home/.local/share/sot"
printf '9.9.8\n' > "$d/ctl/same-out"; printf '4\n' > "$d/ctl/same-rc"
entry_archive "$d" --be-only --no-service
check "a daemon a window holds, on the old version: exit 0" "0" "$ENTRY_RC"
starts_with "and it is ONE STEP LEFT, by hand since this install has no launcher" "ONE STEP LEFT: restart the sotd on $d/ctl/sot.sock by hand" "$ENTRY_LAST"
check "and it is not restarted: no systemctl restart" "0" "$(count_in "$d/ctl/systemctl.log" 'restart')"

# ---------------------------------------------------------------------------
case_start "entry_never_reads_a_running_daemon_that_cannot_be_told_as_none"
# A daemon runs when its unit is active or a process of this account runs with argv0 PREFIX/bin/sotd, never because a
# socket file exists. One that runs and cannot be told its version (same-version exits 2) is unknown: the ask stops the
# run with sotd's line and changes nothing, and the end check beside it is not a PASS.
WHY='sotd same-version: no answer within 5 s'
d="$WORK/unk-proc"; entry_fixture "$d"; P="$d/home/.local/share/sot"
entry_record "$d" 9.9.8 1 0 none
printf '%s\n' "$WHY" > "$d/ctl/same-err"; printf '4243 sotd %s/bin/sotd --socket %s/ctl/sot.sock\n4244 sotd /elsewhere/bin/sotd\n' "$P" "$d" > "$d/ctl/ps"
check "the daemon of this install is found by argv0 alone" "4243" "$(CTL="$d/ctl" PATH="$d/stubs:$ENTRY_TOOLS" installer_daemon_running "$P" | tr '\n' ' ' | sed 's/ $//')"
before="$(entry_prefix_tree "$d")"
entry_archive "$d" --be-only --no-service
check "a process that runs and cannot be told: exit 2, one verdict, the socket and sotd's line, nothing changed" "2 1 FAIL: daemon: a daemon runs on $d/ctl/sot.sock and could not be told its version ($WHY) $before" "$ENTRY_RC $(printf '%s\n' "$ENTRY_OUT" | grep -c -E '^(PASS|FAIL|ONE STEP LEFT):' || true) $ENTRY_LAST $(entry_prefix_tree "$d")"
# No process, a stale socket file a crash left, and an answer of 2: none, as before.
d="$WORK/unk-stale"; entry_fixture "$d"; entry_record "$d" 9.9.8 1 0 none
printf '%s\n' "$WHY" > "$d/ctl/same-err"; : > "$d/ctl/sot.sock"
entry_archive "$d" --be-only --no-service
check "a stale socket file and no process is no daemon: the run goes on to PASS" "0 PASS" "$ENTRY_RC $(printf '%s' "$ENTRY_LAST" | cut -c1-4)"
# An active unit beside the same silence: the ask stops the run, --restart-daemon answers it, and the end check names
# the daemon that runs and cannot be told (never "no daemon answers").
d="$WORK/unk-unit"; entry_fixture "$d"
entry_record "$d" 9.9.8 1 0 systemd; : > "$d/ctl/unit-active"; printf '%s\n' "$WHY" > "$d/ctl/same-err"
before="$(entry_prefix_tree "$d")"
entry_archive "$d" SOT_INSTALL_RESTART_WAIT_S=1 --be-only
check "an active unit that cannot be told: exit 2, sotd's line, the way to answer, nothing changed or restarted" "2 1 1 $before 0" "$ENTRY_RC $(said "FAIL: daemon: a daemon runs on $d/ctl/sot.sock and could not be told its version ($WHY)") $(said 're-run with --restart-daemon') $(entry_prefix_tree "$d") $(count_in "$d/ctl/systemctl.log" 'try-restart')"
entry_archive "$d" SOT_INSTALL_RESTART_WAIT_S=1 --be-only --restart-daemon
check "--restart-daemon restarts the unit, and a daemon still silent ends FAIL daemon, never PASS" "1 1" "$ENTRY_RC $(count_in "$d/ctl/systemctl.log" 'try-restart')"
check "the end check says the daemon runs and cannot be told" "FAIL: daemon: sotd.service is active, but the daemon on $d/ctl/sot.sock could not be told its version ($WHY); see journalctl --user -u sotd.service; pinned at 9.9.9: automatic updates stay off until a run that names no version" "$ENTRY_LAST"
# With no unit, a daemon that appears during the run (the comm refresh starts it) and cannot be told is a FAIL at the end.
d="$WORK/unk-after"; entry_fixture "$d"; P="$d/home/.local/share/sot"
entry_record "$d" 9.9.8 1 0 none
printf '%s\n' "$WHY" > "$d/ctl/same-err"
printf 'printf "4243 sotd %%s/.local/share/sot/bin/sotd\\n" "$HOME" > "$CTL/ps"\n' > "$d/ctl/hook"
entry_archive "$d" --be-only --no-service
check "a daemon that runs after the install and cannot be told is FAIL daemon" "1 FAIL: daemon: a daemon runs on $d/ctl/sot.sock and could not be told its version ($WHY); pinned at 9.9.9: automatic updates stay off until a run that names no version" "$ENTRY_RC $ENTRY_LAST"

# ---------------------------------------------------------------------------
case_start "entry_asks_before_it_restarts_a_daemon_with_a_live_repl"
# An owned unit on another version is restarted by the update. When the daemon says a row's REPL is ready, the run
# stops first, names the rows and changes nothing; --restart-daemon is the answer yes. A daemon of this version is
# not restarted at all, so a repair run costs no REPL its state.
d="$WORK/live"; entry_fixture "$d"; P="$d/home/.local/share/sot"
entry_record "$d" 9.9.8 1 0 systemd; : > "$d/ctl/unit-active"; : > "$d/ctl/restart-same"
printf '9.9.8\n' > "$d/ctl/same-out"; printf '3\n' > "$d/ctl/same-rc"
printf 'repl Rig PC\nrepl scan\n' > "$d/ctl/repls-out"; printf '3\n' > "$d/ctl/repls-rc"
before="$(entry_prefix_tree "$d")"
entry_archive "$d" --be-only
check "a live REPL stops the run (exit 2)" "2" "$ENTRY_RC"
check "it names the rows whose REPL would end" "1" "$(said 'FAIL: daemon: the daemon on '"$d"'/ctl/sot.sock runs another version, and restarting it would end the REPLs of: Rig PC, scan')"
check "it says to ask and how to answer" "1" "$(said 'Ask whoever uses this computer, then re-run with --restart-daemon.')"
check "and that nothing under the install was changed" "1" "$(said 'Nothing under the install was changed.')"
check "nothing changed" "$before" "$(entry_prefix_tree "$d")"
check "the daemon was asked of the target's sotd, by its own socket" "1" "$(sotd_log "$d" | grep -c -x -F "sotd update repls --endpoint unix:$d/ctl/sot.sock" || true)"
check "no restart was made" "0" "$(count_in "$d/ctl/systemctl.log" 'try-restart')"
entry_archive "$d" --be-only --restart-daemon
check "--restart-daemon goes on: exit 0, the unit restarted once" "0 1" "$ENTRY_RC $(count_in "$d/ctl/systemctl.log" 'try-restart')"
check "and the verdict is PASS once the daemon runs the target" "PASS" "$(printf '%s' "$ENTRY_LAST" | cut -c1-4)"
# No live REPL: no question, a restart.
d="$WORK/live-none"; entry_fixture "$d"
entry_record "$d" 9.9.8 1 0 systemd; : > "$d/ctl/unit-active"; : > "$d/ctl/restart-same"
printf '9.9.8\n' > "$d/ctl/same-out"; printf '3\n' > "$d/ctl/same-rc"; printf '0\n' > "$d/ctl/repls-rc"
entry_archive "$d" --be-only
check "no live REPL: the update restarts the unit and passes" "0 1 PASS" "$ENTRY_RC $(count_in "$d/ctl/systemctl.log" 'try-restart') $(printf '%s' "$ENTRY_LAST" | cut -c1-4)"
# A daemon that cannot be asked in full fails closed: the restart waits for the person's answer, and the stop says why it
# could not ask and names the REPL rows it could read.
LIVE_PLUTO_OLD="sotd update repls: the daemon's reply says nothing of its Pluto server (a daemon older than this build cannot say whether it is running)"
d="$WORK/live-unknown"; entry_fixture "$d"
entry_record "$d" 9.9.8 1 0 systemd; : > "$d/ctl/unit-active"; : > "$d/ctl/restart-same"
printf '9.9.8\n' > "$d/ctl/same-out"; printf '3\n' > "$d/ctl/same-rc"
printf 'repl Rig PC\n' > "$d/ctl/repls-out"; printf '%s\n' "$LIVE_PLUTO_OLD" > "$d/ctl/repls-err"; printf '1\n' > "$d/ctl/repls-rc"
before="$(entry_prefix_tree "$d")"
entry_archive "$d" --be-only
check "an rc9.13-shaped reply (rows, no Pluto field): exit 2, no restart" "2 0" "$ENTRY_RC $(count_in "$d/ctl/systemctl.log" 'try-restart')"
check "the stop names its REPL rows and says the daemon cannot say whether its Pluto server is running" "1" "$(said "FAIL: daemon: could not ask the daemon on $d/ctl/sot.sock in full: it runs another version, and restarting it would end the REPLs of: Rig PC; $LIVE_PLUTO_OLD")"
check "nothing changed" "$before" "$(entry_prefix_tree "$d")"
check "it says --restart-daemon answers it, and that nothing under the install changed" "1 1" "$(said 're-run with --restart-daemon') $(said 'Nothing under the install was changed')"
entry_archive "$d" --be-only --restart-daemon
check "--restart-daemon answers it: the restart, and PASS" "0 1 PASS" "$ENTRY_RC $(count_in "$d/ctl/systemctl.log" 'try-restart') $(printf '%s' "$ENTRY_LAST" | cut -c1-4)"
check "and it notes why it could not ask in full" "1" "$(said "note: could not ask the daemon on $d/ctl/sot.sock in full ($LIVE_PLUTO_OLD)")"
# A protocol refusal and a silence: each stop carries the sotd's own line, and no REPL rows (none could be read).
d="$WORK/live-refused"; entry_fixture "$d"
entry_record "$d" 9.9.8 1 0 systemd; : > "$d/ctl/unit-active"; : > "$d/ctl/restart-same"
printf '9.9.8\n' > "$d/ctl/same-out"; printf '3\n' > "$d/ctl/same-rc"; : > "$d/ctl/repls-out"
printf 'sotd update repls: unix:/x: the daemon refused the hello (it speaks protocol 2, this sotd 3)\n' > "$d/ctl/repls-err"; printf '1\n' > "$d/ctl/repls-rc"
entry_archive "$d" --be-only
check "a protocol refusal: exit 2, no restart, the FAIL carries the refusal" "2 0 1" "$ENTRY_RC $(count_in "$d/ctl/systemctl.log" 'try-restart') $(said 'it runs another version; sotd update repls: unix:/x: the daemon refused the hello (it speaks protocol 2, this sotd 3)')"
printf 'sotd update repls: unix:/x: no reply to workspace.list within 10s\n' > "$d/ctl/repls-err"
entry_archive "$d" --be-only
check "a silence: the FAIL carries the timeout" "2 1" "$ENTRY_RC $(said 'it runs another version; sotd update repls: unix:/x: no reply to workspace.list within 10s')"
: > "$d/ctl/repls-err"
entry_archive "$d" --be-only
check "a sotd that gives no reason: the FAIL names its exit status" "2 1" "$ENTRY_RC $(said 'sotd update repls gave no reason (exit 1)')"
# The daemon already runs the target: nothing is asked and nothing is restarted.
d="$WORK/live-same"; entry_fixture "$d"
entry_record "$d" 9.9.9 1 0 systemd; : > "$d/ctl/unit-active"; printf '0\n' > "$d/ctl/same-rc"; printf 'repl Rig PC\n' > "$d/ctl/repls-out"; printf '3\n' > "$d/ctl/repls-rc"
entry_archive "$d" --be-only
check "a daemon on the target version: no question, no restart, PASS" "0 0 0 PASS" \
    "$ENTRY_RC $(count_in "$d/ctl/systemctl.log" 'try-restart') $(sotd_log "$d" | grep -c 'update repls' || true) $(printf '%s' "$ENTRY_LAST" | cut -c1-4)"
# A unit that does not come back is a failed check.
d="$WORK/live-down"; entry_fixture "$d"
entry_record "$d" 9.9.8 1 0 systemd; : > "$d/ctl/unit-active"; printf '9.9.8\n' > "$d/ctl/same-out"; printf '3\n' > "$d/ctl/same-rc"; printf '0\n' > "$d/ctl/repls-rc"
cat > "$d/stubs/systemctl" <<'SC'
#!/bin/sh
echo "systemctl $*" >> "$CTL/systemctl.log"
case "$*" in
    "--user is-active"*) [ -f "$CTL/unit-active" ] && [ ! -f "$CTL/restarted" ]; exit $? ;;
    "--user cat sotd.service") printf "ExecStart=/bin/bash -c 'exec \"%s/bin/sotd\"'\n" "$HOME/.local/share/sot" ;;
    *try-restart*) : > "$CTL/restarted"; echo 3 > "$CTL/same-rc" ;;
esac
exit 0
SC
entry_archive "$d" SOT_INSTALL_RESTART_WAIT_S=1 --be-only
check "a unit that is not running after its restart: exit 1" "1" "$ENTRY_RC"
starts_with "and the last line says so" "FAIL: daemon: sotd.service is not running after the restart" "$ENTRY_LAST"

# ---------------------------------------------------------------------------
case_start "entry_names_a_window_that_runs_the_old_version"
# A window of this account, found before the swap, is ONE STEP LEFT when the version changed, and nothing on a repair run.
d="$WORK/win"; entry_fixture "$d"; P="$d/home/.local/share/sot"
entry_record "$d" 9.9.8 1 1 none
printf '4242 sot %s/bin/sot --socket /x/sot.sock\n4243 sotd %s/bin/sotd --socket /x\n4244 sot /elsewhere/bin/sot\n' "$P" "$P" > "$d/ctl/ps"; printf '0\n' > "$d/ctl/same-rc"
check "the windows of this install are found by argv0 alone" "4242" "$(CTL="$d/ctl" PATH="$d/stubs:$ENTRY_TOOLS" installer_windows_running "$P" | tr '\n' ' ' | sed 's/ $//')"
entry_archive "$d" --no-service
check "a window on the old version: ONE STEP LEFT, with the pin in the line" "0 ONE STEP LEFT: quit Ship of Tools and start it again: its window still runs 9.9.8; pinned at 9.9.9: automatic updates stay off until a run that names no version" "$ENTRY_RC $ENTRY_LAST"
entry_archive "$d" --no-service
check "the same run again, at the installed version: PASS" "0 PASS" "$ENTRY_RC $(printf '%s' "$ENTRY_LAST" | cut -c1-4)"

# ---------------------------------------------------------------------------
case_start "entry_installs_offline_from_a_carried_folder"
# --archive reads the folder and the base clone's origin and nothing else: every curl call fails here, and there are none.
d="$WORK/off"; entry_fixture "$d"
rm -f "$d/stubs/curl"; printf '#!/bin/sh\necho "curl $*" >> "$CTL/net.log"\nexit 7\n' > "$d/stubs/curl"; chmod +x "$d/stubs/curl"
entry_archive "$d" --be-only --no-service
check "an offline archive install passes" "0 PASS" "$ENTRY_RC $(printf '%s' "$ENTRY_LAST" | cut -c1-4)"
check "it made no curl call" "no" "$([ -s "$d/ctl/net.log" ] && echo yes || echo no)"
check "the checkout is the folder's commit" "$(cat "$d/ctl/commit")" "$(git -C "$d/home/.local/share/sot/repo/current" rev-parse HEAD)"
# Carried to a machine that cannot reach the origin, the same folder installs when the base clone already holds its commit.
mv "$d/origin" "$d/origin.gone"
entry_archive "$d" --be-only --no-service
check "an unreachable origin, the commit already in the base clone: PASS" "0 PASS 1" "$ENTRY_RC $(printf '%s' "$ENTRY_LAST" | cut -c1-4) $(said 'could not fetch origin')"


# ---------------------------------------------------------------------------
case_start "entry_repeats_and_resumes_after_an_interrupted_run"
# A run that stops after the swap (comm fails, on both of julia_run's attempts) leaves new binaries over the old checkout and the old install.json; running
# it again finishes it, and a third run on the finished install is a repair run that passes.
d="$WORK/rep"; entry_fixture "$d"; P="$d/home/.local/share/sot"
entry_record "$d" 9.9.8 1 0 none
echo 2 > "$d/ctl/julia-fail"
entry_archive "$d" --no-service
check "the interrupted run fails in comm: exit 1" "1" "$ENTRY_RC"
starts_with "its last line is the FAIL" "FAIL: comm:" "$ENTRY_LAST"
check "install.json still names the old version, since it is written last" "9.9.8" "$(entry_installed "$d")"
entry_archive "$d" --no-service
check "the repeat finishes it: PASS, and install.json names the new version" "0 PASS 9.9.9" "$ENTRY_RC $(printf '%s' "$ENTRY_LAST" | cut -c1-4) $(entry_installed "$d")"
before="$(entry_prefix_tree "$d" | grep -v 'install.json' | grep -v '\.prev')"
entry_archive "$d" --no-service
check "a repair run on the finished install passes" "0 PASS" "$ENTRY_RC $(printf '%s' "$ENTRY_LAST" | cut -c1-4)"

# ---------------------------------------------------------------------------
case_start "entry_fails_a_backend_install_whose_hub_omits_this_box"
# --backend <alias> with no topology entry reaches its backend through the hub's hosts.toml. When the hub omits this
# box the run ends FAIL naming the command to run on the hub, instead of leaving a window with no hosts.
d="$WORK/enr"; entry_fixture "$d"
printf '3\n' > "$d/ctl/sync-rc"; printf "sotd topology: hub's hosts.toml does not list this box; add [host.entrybox] on the hub\n" > "$d/ctl/sync-out"
entry_archive "$d" --backend hubbox
check "the hub omits this box: FAIL enroll, exit 1" "1" "$ENTRY_RC"
check "it names the hub-side command with this box's name" "1" "$(said "FAIL: enroll: the hosts.toml on 'hubbox' does not list this computer ('entrybox'), so the window would open with no hosts: on 'hubbox' run  sotd topology set add entrybox --frontend")"
check "it is the last line" "yes" "$(case "$ENTRY_LAST" in ('FAIL: enroll:'*) echo yes ;; (*) echo no ;; esac)"
check "it does not say this computer's own rows are missing" "0" "$(said "own rows do not show")"
check "the sync asked the alias it was given" "1" "$(sotd_log "$d" | grep -c -x -F 'sotd topology sync --hub hubbox' || true)"
printf '0\n' > "$d/ctl/sync-rc"; : > "$d/ctl/sync-out"
entry_archive "$d" --backend hubbox
check "the hub lists this box: PASS" "0 PASS" "$ENTRY_RC $(printf '%s' "$ENTRY_LAST" | cut -c1-4)"
check "a --backend install's launcher is the remote one" "yes" "$(grep -q 'launch-sot.sh' "$d/home/.local/bin/sot-launch" && grep -q '^export SOT_HOST="hubbox"$' "$d/home/.local/bin/sot-launch" && echo yes || echo no)"
check "and it runs no daemon of its own: no SOT_OWN_DAEMON" "0" "$(grep -c 'SOT_OWN_DAEMON' "$d/home/.local/bin/sot-launch" || true)"
printf '255\n' > "$d/ctl/sync-rc"; printf 'ssh: Could not resolve hostname hubbox\n' > "$d/ctl/sync-out"; rm -f "$d/ctl/plan"
entry_archive "$d" --backend hubbox
check "a hub that cannot be read, with no plan here: FAIL enroll naming why" "1 FAIL: enroll: could not read the hosts.toml on 'hubbox': ssh: Could not resolve hostname hubbox; pinned at 9.9.9: automatic updates stay off until a run that names no version" "$ENTRY_RC $ENTRY_LAST"
printf 'self entrybox\nhub hubbox\nrelay-endpoint x\ndial hubbox ssh:hubbox\n' > "$d/ctl/plan"
entry_archive "$d" --backend hubbox
check "a hub that cannot be reached, but a plan that lists this box: PASS" "0 PASS" "$ENTRY_RC $(printf '%s' "$ENTRY_LAST" | cut -c1-4)"

# ---------------------------------------------------------------------------
case_start "entry_keeps_an_old_version_folder_that_has_local_changes"
# The prune removes the older clean version folders and names and keeps a dirty one until it is clean.
d="$WORK/prune"; entry_fixture "$d"; P="$d/home/.local/share/sot"; B="$P/repo/base"
for n in v0.1.0 v0.2.0 v0.3.0; do git -C "$B" worktree add -q --detach "$P/repo/versions/$n" v9.9.8; done
ln -sfn "$P/repo/versions/v0.3.0" "$P/repo/current"
printf 'my edit\n' >> "$P/repo/versions/v0.1.0/logo.png"
: > "$P/repo/versions/v0.2.0/Manifest.toml"
entry_archive "$d" --be-only --no-service
check "the run passes" "0 PASS" "$ENTRY_RC $(printf '%s' "$ENTRY_LAST" | cut -c1-4)"
check "the dirty two-back folder survives, edit intact" "my edit" "$(tail -1 "$P/repo/versions/v0.1.0/logo.png")"
check "it says which folder and why" "1" "$(said 'keeping old version dir v0.1.0: it has local changes (')"
check "the clean old folder (its ignored Manifest.toml aside) is pruned" "no" "$([ -e "$P/repo/versions/v0.2.0" ] && echo yes || echo no)"
check "the previously active folder is kept for rollback" "yes" "$([ -d "$P/repo/versions/v0.3.0" ] && echo yes || echo no)"
check "the new version's folder is in place" "$(cat "$d/ctl/commit")" "$(git -C "$P/repo/versions/v9.9.9" rev-parse HEAD)"
# A repair run (repo/current already at the target) prunes nothing; the next update names the dirty one again.
entry_archive "$d" --be-only --no-service
check "a repair run prunes nothing and says nothing of it" "yes 0" "$([ -d "$P/repo/versions/v0.1.0" ] && echo yes || echo no) $(said 'old version dir')"
ln -sfn "$P/repo/versions/v0.3.0" "$P/repo/current"
entry_archive "$d" --be-only --no-service
check "the next update names it again and keeps it" "yes 1" "$([ -d "$P/repo/versions/v0.1.0" ] && echo yes || echo no) $(said 'keeping old version dir v0.1.0')"
git -C "$P/repo/versions/v0.1.0" checkout -q -- logo.png
ln -sfn "$P/repo/versions/v0.3.0" "$P/repo/current"
entry_archive "$d" --be-only --no-service
check "once it is clean the next update prunes it" "no" "$([ -e "$P/repo/versions/v0.1.0" ] && echo yes || echo no)"

# ---------------------------------------------------------------------------
case_start "entry_gives_an_enrolled_frontend_the_launcher_that_dials_the_plan"
# A laptop the hub's hosts.toml lists as a frontend, re-running the installer with no --backend: its launcher dials every
# host of the plan (launch-sot.sh), not this computer's own daemon alone. With no other daemon host in the plan it
# stays the all-in-one one.
d="$WORK/lap"; entry_fixture "$d"; P="$d/home/.local/share/sot"
printf 'hubbox hub,daemon\ngpubox daemon\nentrybox frontend\n' > "$d/ctl/topo-status"
printf 'self entrybox\nhub hubbox\nrelay-endpoint ssh:hubbox\ndial hubbox ssh:hubbox\ndial gpubox ssh:hubbox/gpubox\n' > "$d/ctl/plan"
entry_record "$d" 9.9.8 1 1 none
entry_archive "$d" --no-service
W="$d/home/.local/bin/sot-launch"
check "the run passes" "0 PASS" "$ENTRY_RC $(printf '%s' "$ENTRY_LAST" | cut -c1-4)"
check "the launcher is the one that execs the checkout's launch-sot.sh" "yes" "$(grep -q "exec \"$P/repo/current/scripts/launch-sot.sh\"" "$W" && echo yes || echo no)"
check "it names no backend alias (the plan lists the hosts)" "0" "$(grep -c 'SOT_HOST' "$W" || true)"
check "it says why" "1" "$(said 'the topology plan lists other daemon hosts')"
check "and that this computer's own daemon is dialed beside them" "1" "$(said "and this computer's own daemon beside them")"
check "it tells the launcher this install runs a daemon of its own" "1" "$(grep -c '^export SOT_OWN_DAEMON=1$' "$W" || true)"
# The launcher, run: the frontend is handed this computer's own socket and one --dial for each host of the plan. The
# plan does not list this computer as a daemon host, and its own rows show all the same (as on Windows).
launch() { : > "$d/ctl/sot.log"; ( env -i HOME="$d/home" PATH="$d/stubs:$ENTRY_TOOLS" CTL="$d/ctl" SOT_COMM_HOME="$d/comm" SOT_SELF_HOST=entrybox "$W" ) > "$d/launch.out" 2>&1 || true; }
launch
check "the frontend gets --socket and a --dial for every host of the plan" "sot --socket $d/ctl/sot.sock --dial hubbox=ssh:hubbox --dial gpubox=ssh:hubbox/gpubox" "$(tail -1 "$d/ctl/sot.log")"
check "this computer's own daemon was started on that socket" "1" "$(sotd_log "$d" | grep -c -F -e "sotd --socket $d/ctl/sot.sock --project-root" || true)"
# A box that is both a window and a daemon host of the plan: its own socket once, no dial to itself.
printf 'self entrybox\nhub hubbox\nrelay-endpoint ssh:hubbox\ndial hubbox ssh:hubbox\ndial entrybox unix:%s/ctl/sot.sock\n' "$d" > "$d/ctl/plan"
launch
check "a plan that lists this computer too: --socket once and no dial to itself" "sot --socket $d/ctl/sot.sock --dial hubbox=ssh:hubbox" "$(tail -1 "$d/ctl/sot.log")"
printf 'self entrybox\nhub hubbox\nrelay-endpoint ssh:hubbox\ndial hubbox ssh:hubbox\ndial gpubox ssh:hubbox/gpubox\n' > "$d/ctl/plan"
printf 'self entrybox\nhub entrybox\nrelay-endpoint unix:/x\ndial entrybox unix:/x\n' > "$d/ctl/plan"
entry_archive "$d" --no-service
check "a plan with only this computer's own daemon keeps the all-in-one launcher" "yes" "$(grep -q '^# sot-launch: all-in-one$' "$W" && echo yes || echo no)"
check "the pure choice: an alias or another host makes it remote" "remote remote all-in-one" \
    "$(installer_launcher_kind hub '') $(installer_launcher_kind '' yes) $(installer_launcher_kind '' '')"
check "the plan's other hosts are read apart from its own" "yes|" \
    "$(printf 'self a\ndial a unix:/x\ndial b ssh:h\n' | installer_plan_dials_others)|$(printf 'self a\ndial a unix:/x\n' | installer_plan_dials_others)"

# ---------------------------------------------------------------------------
case_start "entry_asks_the_real_sotd_which_way_and_which_rows"
# The two answers install.sh reads, from the real sotd on a real install.json (SOT_TEST_SOTD, else this tree's debug build).
RS="${SOT_TEST_SOTD:-${CARGO_TARGET_DIR:-$ENTRY_SRC/rust/target}/debug/sotd}"
if [ -x "$RS" ]; then
    d="$WORK/real"; mkdir -p "$d/p"
    installer_manifest_json "$d/p" "$d/c" none 0.6.6-rc9.14-dev+a 'v0.6.6-rc9.14-dev+a' 0000 2026-10-01T00:00:00Z "" 1 0 > "$d/p/install.json"
    ask() { "$RS" update direction "$1" --prefix "$d/p" 2>/dev/null || echo "exit $?"; }
    check "the real sotd: a dev build is above the candidate it follows, below the next one" "older newer same" "$(ask 0.6.6-rc9.13) $(ask 0.6.6-rc9.14) $(ask v0.6.6-rc9.14-dev+a)"
    rm -f "$d/p/install.json"; check "no install.json: exit 1" "exit 1" "$(ask 0.6.6)"
    rc=0; "$RS" update repls --endpoint "unix:$d/none.sock" >/dev/null 2>&1 || rc=$?; check "no daemon to ask: exit 1" "1" "$rc"
    # An enrolled frontend runs a private daemon of its own, which the plan does not list unless the host is also a daemon
    # host: the launcher hands the window its socket itself (the lap case).
    printf 'hub = "hubbox"\n\n[host.hubbox]\ndaemon = true\n\n[host.laptop]\nfrontend = true\n' > "$d/hosts.toml"
    check "the real plan: a frontend-only host dials the hub and not itself" "dial hubbox ssh:hubbox" \
        "$(env SOT_HOSTS="$d/hosts.toml" HOME="$d" "$RS" topology plan --self laptop | grep '^dial' | tr '\n' ';' | sed 's/;$//')"
else
    printf '    SKIP the real sotd cases (no built sotd at %s; build it with cargo build -p sot-backend --bin sotd)\n' "$RS"
fi

# ---------------------------------------------------------------------------
case_start "entry_does_not_pass_a_daemon_that_never_answers"
# A restart, and a start of the unit, are verified: PASS needs the daemon on the socket to run the target.
d="$WORK/ver-restart"; entry_fixture "$d"
entry_record "$d" 9.9.8 1 0 systemd; : > "$d/ctl/unit-active"
printf '9.9.8\n' > "$d/ctl/same-out"; printf '3\n' > "$d/ctl/same-rc"; printf '0\n' > "$d/ctl/repls-rc"
entry_archive "$d" SOT_INSTALL_RESTART_WAIT_S=1 --be-only
check "a restart after which the old daemon still answers: exit 1, restarted once" "1 1" "$ENTRY_RC $(count_in "$d/ctl/systemctl.log" 'try-restart')"
starts_with "its last line is FAIL: daemon" "FAIL: daemon:" "$ENTRY_LAST"
printf '2\n' > "$d/ctl/same-rc"
entry_archive "$d" SOT_INSTALL_RESTART_WAIT_S=1 --be-only
starts_with "a restart after which nothing answers: FAIL: daemon" "FAIL: daemon:" "$ENTRY_LAST"
check "and the verdict is the only one" "1" "$(printf '%s\n' "$ENTRY_OUT" | grep -c -E '^(PASS|FAIL|ONE STEP LEFT):' || true)"
# A unit that is active but whose daemon cannot be told (same-version exit 2) is unknown: the ask stops the run before
# anything is restarted (entry_never_reads_a_running_daemon_that_cannot_be_told_as_none has the rest).
d="$WORK/ver-told"; entry_fixture "$d"
entry_record "$d" 9.9.8 1 0 systemd; : > "$d/ctl/unit-active"; printf '2\n' > "$d/ctl/same-rc"
entry_archive "$d" SOT_INSTALL_RESTART_WAIT_S=1 --be-only
check "an active unit whose daemon cannot be told: not restarted, exit 2" "0 2" "$(count_in "$d/ctl/systemctl.log" 'try-restart') $ENTRY_RC"
starts_with "and FAIL: daemon" "FAIL: daemon:" "$ENTRY_LAST"
# A unit the run starts: the daemon must answer after enable --now.
d="$WORK/ver-fresh"; entry_fixture "$d"
entry_archive "$d" SOT_INSTALL_RESTART_WAIT_S=1 --be-only
check "a fresh unit whose daemon never answers: exit 1, FAIL: daemon" "1 FAIL: daemon" "$ENTRY_RC $(printf '%s' "$ENTRY_LAST" | cut -c1-12)"
: > "$d/ctl/start-same"; rm -f "$d/ctl/unit-active"; printf '2\n' > "$d/ctl/same-rc"
entry_archive "$d" SOT_INSTALL_RESTART_WAIT_S=1 --be-only
check "a fresh unit whose daemon answers as the target: PASS" "0 PASS" "$ENTRY_RC $(printf '%s' "$ENTRY_LAST" | cut -c1-4)"
# The wait is bounded by the clock: a daemon that accepts and never answers costs each ask its own bound (3 s here), and the
# wait still ends within the bound and one more ask, not after one ask per second of the bound.
d="$WORK/ver-clock"; mkdir -p "$d"
printf '#!/bin/sh\nsleep 3\nexit 2\n' > "$d/sotd"; chmod +x "$d/sotd"
t0="$(date +%s)"; st="$(installer_daemon_wait "$d/sotd" "$d/none.sock" 4 0)"; t1="$(date +%s)"
check "a daemon that never answers: none, within the bound and one ask" "none yes" "$st $([ $((t1 - t0)) -le 8 ] && echo yes || echo "no ($((t1 - t0)) s)")"
# A daemon install whose host pin excludes this computer: the unit never starts, and the FAIL names the pin.
d="$WORK/ver-pin"; entry_fixture "$d"
printf 'hubbox hub,daemon\n' > "$d/ctl/topo-status"; : > "$d/ctl/pin-excludes"
entry_archive "$d" SOT_INSTALL_RESTART_WAIT_S=1 --be-only
check "a pin that excludes this computer: exit 1" "1" "$ENTRY_RC"
check "the FAIL names the pin as the cause" "1" "$(said "FAIL: daemon: sotd.service is not running because its host pin excludes this computer ('entrybox')")"

# ---------------------------------------------------------------------------
case_start "entry_names_the_rows_a_step_would_end_and_the_command_that_ends_them"
# A daemon on another version that this run does not restart (a window holds it, or the launcher started it) is asked its
# live rows too, and the ONE STEP LEFT names them and the command that fits this computer.
check "the step: a launcher, a unit, or by hand" "start Ship of Tools again (the daemon on S still runs another version, beside the new repo/current and bin/sot-capsule); starting it again ends the REPLs of: A, B|run systemctl --user restart sotd.service once the window that holds the daemon on S is closed (it still runs another version, beside the new repo/current and bin/sot-capsule); starting it again ends the REPLs of: A|restart the sotd on S by hand (it still runs another version, beside the new repo/current and bin/sot-capsule); its live REPLs and Pluto server could not be listed, so starting it again may end some" \
    "$(installer_daemon_step launcher other S 'A, B' 0 0)|$(installer_daemon_step unit held S A 0 0)|$(installer_daemon_step hand other S '' 0 1)"
# B: an owned unit a window holds, be-only (no launcher), a live REPL.
d="$WORK/held-rows"; entry_fixture "$d"
entry_record "$d" 9.9.8 1 0 systemd; : > "$d/ctl/unit-active"
printf '9.9.8\n' > "$d/ctl/same-out"; printf '4\n' > "$d/ctl/same-rc"; printf 'repl Rig PC\n' > "$d/ctl/repls-out"; printf '3\n' > "$d/ctl/repls-rc"
entry_archive "$d" --be-only
check "a held unit is asked its rows, and not restarted" "0 1 0" "$ENTRY_RC $(sotd_log "$d" | grep -c 'update repls' || true) $(count_in "$d/ctl/systemctl.log" 'try-restart')"
check "the step is the systemctl command, with the row it ends, and no launcher" "1 1 0" "$(said 'ONE STEP LEFT: run systemctl --user restart sotd.service') $(said 'starting it again ends the REPLs of: Rig PC') $(printf '%s' "$ENTRY_LAST" | grep -c -F 'Ship of Tools' || true)"
# The same unit on a box with a launcher and a local window: the launcher is the step, and the window step names the rows.
entry_record "$d" 9.9.8 1 1 systemd
P="$d/home/.local/share/sot"
printf '4242 sot %s/bin/sot --socket /x/sot.sock\n' "$P" > "$d/ctl/ps"
entry_archive "$d"
starts_with "a held unit with a local window: quit and start it again, naming the rows" "ONE STEP LEFT: quit Ship of Tools and start it again: its window still runs 9.9.8 (starting it again ends the REPLs of: Rig PC)" "$ENTRY_LAST"
rm -f "$d/ctl/ps"
entry_record "$d" 9.9.8 1 1 systemd
entry_archive "$d"
check "a held unit with a launcher but no local window: the unit's command, not the launcher" "1" "$(said 'ONE STEP LEFT: run systemctl --user restart sotd.service')"
# E: a daemon the launcher started (--no-service) on another version, with a live REPL.
d="$WORK/nosvc-rows"; entry_fixture "$d"
entry_record "$d" 9.9.8 1 0 none
printf '9.9.8\n' > "$d/ctl/same-out"; printf '3\n' > "$d/ctl/same-rc"; printf 'repl Rig PC\n' > "$d/ctl/repls-out"; printf '3\n' > "$d/ctl/repls-rc"
entry_archive "$d" --be-only --no-service
check "a daemon with no unit is asked its rows and left alone: exit 0" "0 1" "$ENTRY_RC $(sotd_log "$d" | grep -c 'update repls' || true)"
check "the step is by hand and names the row" "1 1" "$(said "ONE STEP LEFT: restart the sotd on $d/ctl/sot.sock by hand") $(said 'starting it again ends the REPLs of: Rig PC')"
entry_record "$d" 9.9.8 1 1 none
entry_archive "$d" --no-service
check "with a launcher the step is to start Ship of Tools again, naming the row" "1 1" "$(said 'ONE STEP LEFT: start Ship of Tools again') $(said 'starting it again ends the REPLs of: Rig PC')"
# A daemon that cannot be asked, and is not restarted: the step says the rows could not be listed.
: > "$d/ctl/repls-out"; printf '1\n' > "$d/ctl/repls-rc"
entry_record "$d" 9.9.8 1 0 none
entry_archive "$d" --be-only --no-service
check "an unaskable daemon that is left alone: exit 0, the step says the rows could not be listed" "0 1" "$ENTRY_RC $(said 'could not be listed')"
printf 'repl Rig PC\n' > "$d/ctl/repls-out"
entry_archive "$d" --be-only --no-service
check "one that listed a row and could not say the rest: the step names the row and the gap" "0 1" "$ENTRY_RC $(said 'starting it again ends the REPLs of: Rig PC; it could not say the rest')"

# ---------------------------------------------------------------------------
case_start "entry_asks_whether_the_pluto_server_is_running_as_it_asks_about_repls"
# A restart ends the Pluto server and every notebook open in it, however each was opened (Pluto's own home page opens some
# the daemon never sees): one question on the REPLs' path, and the fact is that the server is running.
PL="the Pluto server is running: restarting the daemon ends every notebook open in it"
check "the live work, in words" "restarting it would end the REPLs of: A|$PL|restarting it would end the REPLs of: A; $PL|" \
    "$(installer_live_work A 0 'restarting it would end')|$(installer_live_work '' 1 'restarting it would end')|$(installer_live_work A 1 'restarting it would end')|$(installer_live_work '' 0 'restarting it would end')"
check "the step carries both" "start Ship of Tools again (the daemon on S still runs another version, beside the new repo/current and bin/sot-capsule); starting it again ends the REPLs of: A; $PL" \
    "$(installer_daemon_step launcher other S A 1 0)"
check "a daemon that listed some and could not say the rest: the step names what it listed" "starting it again ends the REPLs of: A; it could not say the rest, so starting it again may end more" \
    "$(installer_rows_note A 0 1)"
d="$WORK/pluto-only"; entry_fixture "$d"
entry_record "$d" 9.9.8 1 0 systemd; : > "$d/ctl/unit-active"; : > "$d/ctl/restart-same"
printf '9.9.8\n' > "$d/ctl/same-out"; printf '3\n' > "$d/ctl/same-rc"
printf 'pluto server\n' > "$d/ctl/repls-out"; printf '3\n' > "$d/ctl/repls-rc"
before="$(entry_prefix_tree "$d")"
entry_archive "$d" --be-only
check "a running Pluto server alone (no notebook opened through the daemon) stops the restart (exit 2), changing nothing" "2 0 yes" "$ENTRY_RC $(count_in "$d/ctl/systemctl.log" 'try-restart') $([ "$before" = "$(entry_prefix_tree "$d")" ] && echo yes || echo no)"
check "it says the server is running" "1" "$(said 'FAIL: daemon: the daemon on '"$d"'/ctl/sot.sock runs another version, and '"$PL")"
entry_archive "$d" --be-only --restart-daemon
check "--restart-daemon answers it" "0 1 PASS" "$ENTRY_RC $(count_in "$d/ctl/systemctl.log" 'try-restart') $(printf '%s' "$ENTRY_LAST" | cut -c1-4)"
# A server not started: the daemon prints no Pluto line, so nothing is asked and the unit restarts.
d="$WORK/pluto-none"; entry_fixture "$d"
entry_record "$d" 9.9.8 1 0 systemd; : > "$d/ctl/unit-active"; : > "$d/ctl/restart-same"
printf '9.9.8\n' > "$d/ctl/same-out"; printf '3\n' > "$d/ctl/same-rc"; : > "$d/ctl/repls-out"; printf '0\n' > "$d/ctl/repls-rc"
entry_archive "$d" --be-only
check "no Pluto line and no REPL: the unit restarts and passes" "0 1 PASS" "$ENTRY_RC $(count_in "$d/ctl/systemctl.log" 'try-restart') $(printf '%s' "$ENTRY_LAST" | cut -c1-4)"
# A held unit with the server running and a REPL: the step names both.
d="$WORK/pluto-held"; entry_fixture "$d"
entry_record "$d" 9.9.8 1 0 systemd; : > "$d/ctl/unit-active"
printf '9.9.8\n' > "$d/ctl/same-out"; printf '4\n' > "$d/ctl/same-rc"
printf 'repl Rig PC\npluto server\n' > "$d/ctl/repls-out"; printf '3\n' > "$d/ctl/repls-rc"
entry_archive "$d" --be-only
check "a held unit's step names the REPLs and the running Pluto server" "0 1" "$ENTRY_RC $(said "starting it again ends the REPLs of: Rig PC; $PL")"

# ---------------------------------------------------------------------------
case_start "entry_keeps_the_rollback_through_a_repair_run"
# An update leaves the previous version's folder and the .prev binaries; a repair run on the finished install keeps them.
d="$WORK/rb"; entry_fixture "$d"; P="$d/home/.local/share/sot"; B="$P/repo/base"
git -C "$B" worktree add -q --detach "$P/repo/versions/v9.9.8" v9.9.8
ln -sfn "$P/repo/versions/v9.9.8" "$P/repo/current"
mkdir -p "$P/bin"; printf '#!/bin/sh\necho old sotd 9.9.8\n' > "$P/bin/sotd"; chmod +x "$P/bin/sotd"
entry_record "$d" 9.9.8 1 0 none
entry_archive "$d" --be-only --no-service
check "the update keeps the previous folder and the old binary as .prev" "0 yes 1" "$ENTRY_RC $([ -d "$P/repo/versions/v9.9.8" ] && echo yes || echo no) $(grep -c 'old sotd 9.9.8' "$P/bin/sotd.prev" || true)"
for run in repair again; do
    entry_archive "$d" --be-only --no-service
    check "a $run run passes and keeps the previous folder" "0 yes" "$ENTRY_RC $([ -d "$P/repo/versions/v9.9.8" ] && echo yes || echo no)"
    check "and the old binary as .prev, which is not the installed one" "1 no" "$(grep -c 'old sotd 9.9.8' "$P/bin/sotd.prev" || true) $(cmp -s "$P/bin/sotd" "$P/bin/sotd.prev" && echo yes || echo no)"
done
# An --archive hop to another commit of the same version is not a repair run.
d="$WORK/hop"; entry_fixture "$d"
entry_record "$d" 9.9.9 1 0 none; sed -i 's/"commit": *"[^"]*"/"commit": "1111111111111111111111111111111111111111"/' "$d/home/.local/share/sot/install.json"
printf 'same\n' > "$d/ctl/direction"
entry_archive "$d" --be-only --no-service
check "same version, another commit: it says so, not a repair run" "1 0" "$(said 'same version, another build') $(said 'a repair run')"

# ---------------------------------------------------------------------------
case_start "entry_ends_every_exit_in_one_verdict_line"
# One EXIT trap prints FAIL when no verdict was printed: option errors, a command that fails under set -e, and an interrupt.
verdicts() { printf '%s\n' "$ENTRY_OUT" | grep -c -E '^(PASS|FAIL|ONE STEP LEFT):' || true; }
d="$WORK/vl1"; entry_fixture "$d"
entry_archive "$d" --be-only --bogus
check "an unknown option: exit 2, one verdict line, the last" "2 1 yes" "$ENTRY_RC $(verdicts) $(case "$ENTRY_LAST" in ('FAIL: '*'unknown flag: --bogus') echo yes ;; (*) echo no ;; esac)"
d="$WORK/vl2"; entry_fixture "$d"
entry_record "$d" 9.9.8 1 0 systemd
cat > "$d/stubs/systemctl" <<'SC'
#!/bin/sh
echo "systemctl $*" >> "$CTL/systemctl.log"
case "$*" in
    "--user is-active"*) exit 3 ;;
    "--user daemon-reload"|*"enable --now"*) echo "Failed to connect to bus: No medium found" >&2; exit 1 ;;
esac
exit 0
SC
entry_archive "$d" --be-only
check "a user manager that cannot be reached: exit 1, one verdict line, the last" "1 1 FAIL: service: stopped (exit 1)" "$ENTRY_RC $(verdicts) $ENTRY_LAST"
d="$WORK/vl3"; entry_fixture "$d"
entry_run "$d" SOT_INSTALL_TAG=v9.9.9 --be-only --version
check "--version with no value: exit 2, one verdict line" "2 1" "$ENTRY_RC $(verdicts)"
starts_with "and it says which option" "FAIL: " "$ENTRY_LAST"
check "naming it" "1" "$(said '--version needs a value')"
# An interrupt mid-run: the signal ends the run after one verdict line, and the scratch folder is gone.
d="$WORK/vl4"; entry_fixture "$d"; printf 'sleep 2\n' > "$d/ctl/hook"
env -i HOME="$d/home" PATH="$d/stubs:$ENTRY_TOOLS" CTL="$d/ctl" SOT_COMM_HOME="$d/comm" SOT_SELF_HOST=entrybox TMPDIR="$d" \
    bash "$ENTRY_SRC/scripts/install.sh" --archive "$d/rel" --be-only --no-service > "$d/sig.out" 2>&1 &
sig_pid=$!
n=0; while [ "$n" -lt 150 ] && ! grep -q update_comm "$d/ctl/julia.log" 2>/dev/null; do sleep 0.1; n=$((n + 1)); done
kill -TERM "$sig_pid"
sig_rc=0; wait "$sig_pid" || sig_rc=$?
check "a TERM mid-run ends by the signal (143)" "143" "$sig_rc"
check "with one verdict line, the last, naming the stage" "1 FAIL: comm: stopped (exit 143)" "$(grep -c -E '^(PASS|FAIL|ONE STEP LEFT):' "$d/sig.out" || true) $(tail -1 "$d/sig.out")"
check "and its scratch folder is gone" "0" "$(ls "$d" | grep -c '^sot-install\.' || true)"

# ---------------------------------------------------------------------------
case_start "entry_records_say_the_person_picks_the_target"
# The words an agent reads: the target and the direction are the person's to answer, and --archive runs the pack's installer.
for f in agents/claude/sot-install/SKILL.md docs/INSTALL-AGENT.md; do
    check "$f: a version the agent picks is never the answer" "1" "$(grep -c -F 'never a version the agent picks' "$ENTRY_SRC/$f" || true)"
    check "$f: --archive runs the install.sh of the checkout that made the pack" "1" "$(grep -c -F 'the install.sh of the checkout that made the pack' "$ENTRY_SRC/$f" || true)"
done

# ---------------------------------------------------------------------------
case_start "entry_records_name_no_version_in_a_routine_command"
# A routine command (the fallbacks for an install that predates the entry, a fresh install) runs the fetched installer
# with no version, so nothing is pinned. A version appears only as the person's choice: any line that runs install.sh,
# install.ps1 or `bash -s` and carries a -version/--version in any form fails unless it also carries the phrase the
# three files use for that choice.
CHOICE="the person's choice"
version_lines() {  # <file>: the lines that run the installer with a version and do not mark it the person's choice
    grep -E -- '(install\.sh|install\.ps1|bash -s)' "$1" | grep -E -- '-{1,2}[Vv]ersion' | grep -v -F -- "$CHOICE" || true
}
for f in agents/claude/sot-install/SKILL.md docs/INSTALL-AGENT.md docs/src/start/install.md; do
    check "$f: no line that runs the installer carries a version unless it is marked the person's choice" "" "$(version_lines "$ENTRY_SRC/$f")"
    check "$f: every paragraph that names a version to run with says it pins" "0" "$(awk -v RS= '/(--version|-Version) (<|v[0-9X])/ && !/pin/ { n++ } END { print n + 0 }' "$ENTRY_SRC/$f")"
done
# The check itself: every spelling of a routine version is caught, and the marked line is not.
printf '%s\n' 'curl -fsSL .../install.sh | bash -s -- --local --version v0.6.6' 'powershell -File install.ps1 -Version $tag' \
    'bash install.sh --version="v0.6.6"' 'bash -s -- --version v0.6.6 --hub rig' "run install.ps1 with -Version <tag>, the person's choice, which pins" > "$WORK/version-lines.txt"
check "the check catches -Version \$tag, --version=\"v..\", --version v.. and bash -s forms, and passes the person's choice" "4" "$(version_lines "$WORK/version-lines.txt" | wc -l | tr -d ' ')"
check "and none says to run the fetched installer with -Version <tag>" "0" "$(cat "$ENTRY_SRC/docs/INSTALL-AGENT.md" "$ENTRY_SRC/agents/claude/sot-install/SKILL.md" "$ENTRY_SRC/docs/src/start/install.md" | grep -c -F 'with `-Version <tag>`' || true)"

# ---------------------------------------------------------------------------
case_start "entry_pins_an_install_whose_run_named_the_version"
# An operator's downgrade is not undone by the next daily check: a run that names what to install (--version, --archive)
# records the pin in install.json, which the updater's arm guard reads; a run that names none removes it.
d="$WORK/pin1"; entry_fixture "$d"
entry_archive "$d" --be-only --no-service
check "--archive: the record says pinned" "1" "$(grep -c '"pinned": true' "$d/home/.local/share/sot/install.json" || true)"
check "and the verdict line says what that means" "PASS: Ship of Tools 9.9.9 at $(cat "$d/ctl/short") on this computer; pinned at 9.9.9: automatic updates stay off until a run that names no version" "$ENTRY_LAST"
printf 'same\n' > "$d/ctl/direction"
entry_run "$d" SOT_INSTALL_TAG=v9.9.9 SOT_INSTALL_IMPLICIT=1 --be-only --no-service
check "an implicit run removes the pin" "0 0" "$ENTRY_RC $(grep -c 'pinned' "$d/home/.local/share/sot/install.json" || true)"
check "and says nothing of one" "0" "$(said 'pinned at')"
check "an implicit run's PASS line carries no pin" "PASS: Ship of Tools 9.9.9 at $(cat "$d/ctl/short") on this computer" "$ENTRY_LAST"
entry_run "$d" SOT_INSTALL_TAG=v9.9.9 --be-only --no-service
check "a body run on a tag by hand is a named version" "1" "$(grep -c '"pinned": true' "$d/home/.local/share/sot/install.json" || true)"
check "a record without the argument writes no pin" "0" "$(installer_manifest_json /p /c none 1.0.0 v1.0.0 abc 2026-10-17T00:00:00Z '' 1 0 | grep -c pinned || true)"
check "and with it ends the object with the pin" '  "pinned": true }' "$(installer_manifest_json /p /c none 1.0.0 v1.0.0 abc 2026-10-17T00:00:00Z '' 1 0 1 | tail -2 | tr '\n' ' ' | sed 's/ $//')"
# The lab's instrument PCs pin themselves by an explicit run of the version they already run: when the daemon already
# answers as that version, the run writes the pin and restarts nothing.
d="$WORK/pin2"; entry_fixture "$d"
entry_record "$d" 9.9.9 1 0 systemd; : > "$d/ctl/unit-active"; printf '0\n' > "$d/ctl/same-rc"; printf 'repl Rig PC\n' > "$d/ctl/repls-out"; printf '3\n' > "$d/ctl/repls-rc"
check "a record from before the pin has no pin field" "0" "$(grep -c pinned "$d/home/.local/share/sot/install.json" || true)"
entry_archive "$d" --be-only
check "an explicit run of the installed version ends in PASS" "0 PASS" "$ENTRY_RC $(printf '%s' "$ENTRY_LAST" | cut -c1-4)"
check "it writes the pin" "1" "$(grep -c '"pinned": true' "$d/home/.local/share/sot/install.json" || true)"
check "it restarts nothing and asks nothing of the daemon" "0 0" "$(count_in "$d/ctl/systemctl.log" 'restart') $(sotd_log "$d" | grep -c 'update repls' || true)"
. "$(dirname "${BASH_SOURCE[0]}")/installer-path-refusal.sh"
