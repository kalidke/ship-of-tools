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
    "session-socket-path sot") echo "$CTL/sot.sock"; exit 0 ;;
    "update direction") if [ -f "$CTL/direction" ]; then f direction; exit 0; fi; exit 1 ;;
    "update repls") f repls-out; rc repls-rc 0 ;;
    "same-version --endpoint") f same-out; rc same-rc 2 ;;
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
    *"enable --now"*) : > "$CTL/unit-active" ;;
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
entry_record "$d" 0.6.6-rc9.14-dev+abc123 1 0 none
prelude --be-only
check "a build with a + stops (exit 2)" "2" "$ENTRY_RC"
check "it says why and names --version and --archive" "1" "$(said 'FAIL: target: this computer runs 0.6.6-rc9.14-dev+abc123, a build that is not a release: name the target with --version vX.Y.Z, or --archive <folder>')"
check "it ends with that one line" "yes" "$(case "$ENTRY_LAST" in ('FAIL: '*) echo yes ;; (*) echo no ;; esac)"
check "it fetched no installer and no asset" "0" "$(count_in "$d/ctl/net.log" 'raw.githubusercontent\|releases/download\|api.github.com\|releases/latest')"
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
check "and ends in PASS" "PASS: Ship of Tools 9.9.9 at $(cat "$d/ctl/short") on this computer" "$ENTRY_LAST"
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
entry_run "$d" SOT_INSTALL_TAG=v9.9.9 SOT_INSTALL_IMPLICIT=1 --be-only --no-service
check "a sotd that cannot say goes on, and says so" "0 1" "$ENTRY_RC $(said 'could not tell which way')"
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
check "a failed check is FAIL and wins over a step" "FAIL: comm: stale" "$(installer_verdict 1.2.3 abc 'comm: stale' 'quit it')"
d="$WORK/vd"; entry_fixture "$d"; short="$(cat "$d/ctl/short")"
entry_archive "$d" --be-only --no-service
check "a whole install ends PASS, as its one last line" "PASS: Ship of Tools 9.9.9 at $short on this computer|0" "$ENTRY_LAST|$ENTRY_RC"
check "the verdict is the only line that starts with PASS, FAIL or ONE STEP" "1" "$(printf '%s\n' "$ENTRY_OUT" | grep -c -E '^(PASS|FAIL|ONE STEP LEFT):' || true)"
broken() {  # <description> <expected fail prefix> <hook>: a run whose comm refresh leaves the install broken one way
    local dd="$WORK/vd-$RANDOM"; entry_fixture "$dd"; printf '%s\n' "$3" > "$dd/ctl/hook"
    entry_archive "$dd" --be-only --no-service
    check "$1: exit 1" "1" "$ENTRY_RC"
    starts_with "$1: FAIL names the check" "FAIL: $2" "$ENTRY_LAST"
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
starts_with "and it is ONE STEP LEFT" "ONE STEP LEFT: start Ship of Tools again" "$ENTRY_LAST"
check "and it is not restarted: no systemctl restart" "0" "$(count_in "$d/ctl/systemctl.log" 'restart')"

# ---------------------------------------------------------------------------
case_start "entry_asks_before_it_restarts_a_daemon_with_a_live_repl"
# An owned unit on another version is restarted by the update. When the daemon says a row's REPL is ready, the run
# stops first, names the rows and changes nothing; --restart-daemon is the answer yes. A daemon of this version is
# not restarted at all, so a repair run costs no REPL its state.
d="$WORK/live"; entry_fixture "$d"; P="$d/home/.local/share/sot"
entry_record "$d" 9.9.8 1 0 systemd; : > "$d/ctl/unit-active"; : > "$d/ctl/restart-same"
printf '9.9.8\n' > "$d/ctl/same-out"; printf '3\n' > "$d/ctl/same-rc"
printf 'Rig PC\nscan\n' > "$d/ctl/repls-out"; printf '3\n' > "$d/ctl/repls-rc"
before="$(entry_prefix_tree "$d")"
entry_archive "$d" --be-only
check "a live REPL stops the run (exit 2)" "2" "$ENTRY_RC"
check "it names the rows whose REPL would end" "1" "$(said 'FAIL: daemon: the daemon on '"$d"'/ctl/sot.sock runs another version, and its rows with a live REPL are: Rig PC, scan')"
check "it says to ask and how to answer" "1" "$(said 'Ask whoever uses this computer, then re-run with --restart-daemon.')"
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
# A daemon that cannot be asked: a note, then the restart.
d="$WORK/live-unknown"; entry_fixture "$d"
entry_record "$d" 9.9.8 1 0 systemd; : > "$d/ctl/unit-active"; : > "$d/ctl/restart-same"
printf '9.9.8\n' > "$d/ctl/same-out"; printf '3\n' > "$d/ctl/same-rc"; printf '1\n' > "$d/ctl/repls-rc"
entry_archive "$d" --be-only
check "a daemon that cannot list its rows: a note and the restart" "0 1 1" "$ENTRY_RC $(count_in "$d/ctl/systemctl.log" 'try-restart') $(said 'could not ask the daemon')"
# The daemon already runs the target: nothing is asked and nothing is restarted.
d="$WORK/live-same"; entry_fixture "$d"
entry_record "$d" 9.9.9 1 0 systemd; : > "$d/ctl/unit-active"; printf '0\n' > "$d/ctl/same-rc"; printf 'Rig PC\n' > "$d/ctl/repls-out"; printf '3\n' > "$d/ctl/repls-rc"
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
printf '4242 sot %s/bin/sot --socket /x/sot.sock\n4243 sotd %s/bin/sotd --socket /x\n4244 sot /elsewhere/bin/sot\n' "$P" "$P" > "$d/ctl/ps"
check "the windows of this install are found by argv0 alone" "4242" "$(CTL="$d/ctl" PATH="$d/stubs:$ENTRY_TOOLS" installer_windows_running "$P" | tr '\n' ' ' | sed 's/ $//')"
entry_archive "$d" --no-service
check "a window on the old version: ONE STEP LEFT" "0 ONE STEP LEFT: quit Ship of Tools and start it again: its window still runs 9.9.8" "$ENTRY_RC $ENTRY_LAST"
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
check "the sync asked the alias it was given" "1" "$(sotd_log "$d" | grep -c -x -F 'sotd topology sync --hub hubbox' || true)"
printf '0\n' > "$d/ctl/sync-rc"; : > "$d/ctl/sync-out"
entry_archive "$d" --backend hubbox
check "the hub lists this box: PASS" "0 PASS" "$ENTRY_RC $(printf '%s' "$ENTRY_LAST" | cut -c1-4)"
check "a --backend install's launcher is the remote one" "yes" "$(grep -q 'launch-sot.sh' "$d/home/.local/bin/sot-launch" && grep -q '^export SOT_HOST="hubbox"$' "$d/home/.local/bin/sot-launch" && echo yes || echo no)"
printf '255\n' > "$d/ctl/sync-rc"; printf 'ssh: Could not resolve hostname hubbox\n' > "$d/ctl/sync-out"; rm -f "$d/ctl/plan"
entry_archive "$d" --backend hubbox
check "a hub that cannot be read, with no plan here: FAIL enroll naming why" "1 FAIL: enroll: could not read the hosts.toml on 'hubbox': ssh: Could not resolve hostname hubbox" "$ENTRY_RC $ENTRY_LAST"
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
entry_archive "$d" --be-only --no-service
check "a second run names it again and keeps it" "yes 1" "$([ -d "$P/repo/versions/v0.1.0" ] && echo yes || echo no) $(said 'keeping old version dir v0.1.0')"
git -C "$P/repo/versions/v0.1.0" checkout -q -- logo.png
entry_archive "$d" --be-only --no-service
check "once it is clean the next run prunes it" "no" "$([ -e "$P/repo/versions/v0.1.0" ] && echo yes || echo no)"

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
# The launcher, run: the frontend is handed one --dial for each host of the plan.
: > "$d/ctl/sot.log"
( env -i HOME="$d/home" PATH="$d/stubs:$ENTRY_TOOLS" CTL="$d/ctl" SOT_COMM_HOME="$d/comm" SOT_SELF_HOST=entrybox "$W" ) > "$d/launch.out" 2>&1 || true
check "the frontend gets a --dial for every host of the plan" "sot --dial hubbox=ssh:hubbox --dial gpubox=ssh:hubbox/gpubox" "$(tail -1 "$d/ctl/sot.log")"
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
else
    printf '    SKIP the real sotd cases (no built sotd at %s; build it with cargo build -p sot-backend --bin sotd)\n' "$RS"
fi
