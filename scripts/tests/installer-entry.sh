#!/usr/bin/env bash
# installer-entry.sh -- "update sot from the repo" on Unix: the bootstrap (scripts/install.sh) end to end, and sotd
# install's refusals end to end. Sourced by installer-state.sh (its check helpers and $WORK), never run.
#
# The bootstrap runs under a stub curl that serves a fake release from a folder and logs every URL; the pack's sotd is a
# stand-in that says what it was run with. Its guarantees: a tag's own installer runs (the channel picks the tag), every
# file of a pack matches SHA256SUMS before anything of it runs, and every exit ends in one verdict line. The refusals run
# the real sotd (SOT_TEST_SOTD) as the pack's: a refused install writes nothing. Bash 3.2: no mapfile, no assoc arrays.

ENTRY_SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ENTRY_TOOLS="$WORK/entry-tools"; mk_tools "$ENTRY_TOOLS" tar gzip tr ls
# One fake release per tag under $d/rel/<tag>/, served by the stub curl; the tag installer a prelude fetches records the
# environment it was run in.
entry_fixture() {  # <d>
    local d="$1"
    mkdir -p "$d/home" "$d/stubs" "$d/rel"
    : > "$d/net.log"
    cat > "$d/stubs/curl" <<STUB
#!/bin/sh
echo "curl \$*" >> "$d/net.log"
out=""; url=""
while [ \$# -gt 0 ]; do case "\$1" in -o) out="\$2"; shift ;; -w|-H) shift ;; http*) url="\$1" ;; esac; shift; done
case "\$url" in
    */releases/latest) printf 'https://github.com/o/r/releases/tag/%s' "\$(cat "$d/rel/latest")" ;;
    *api.github.com*) printf '[{"tag_name": "%s"}]\n200' "\$(cat "$d/rel/newest")" ;;
    *raw.githubusercontent.com*/scripts/install.sh) t="\${url%/scripts/install.sh}"; t="\${t##*/}"; printf 'echo "tag-installer \$SOT_INSTALL_TAG \$SOT_INSTALL_IMPLICIT \$*" >> "$d/net.log"; echo "PASS: Ship of Tools %s at x on this computer"\n' "\$t" > "\$out" ;;
    */releases/download/*) f="\${url##*/download/}"; cp "$d/rel/\$f" "\$out" 2>/dev/null || exit 22 ;;
    *) exit 6 ;;
esac
STUB
    chmod +x "$d/stubs/curl"
}
# A pack in $d/rel/<tag>/ whose sotd stand-in answers --version and `install --help`, records an install's arguments and
# the pack folder's names, and ends it as STUB_LINE and STUB_RC say. <kind>: good, norun (a sotd that cannot start), old
# (a sotd with no install).
entry_pack() {  # <d> <tag> [kind]
    local d="$1" tag="$2" v="${2#v}" kind="${3:-good}" top r
    top="sot-$v-$TGT"; r="$d/rel/$tag"
    mkdir -p "$r/x/$top"
    case "$kind" in
        norun) printf '#!/bin/sh\necho "loader: GLIBC_2.99 not found (required by sotd)" >&2\nexit 1\n' > "$r/x/$top/sotd" ;;
        old) printf '#!/bin/sh\ncase "$1" in --version) echo "sotd %s (abc 2026-01-01, protocol 2)" ;; *) echo "Usage: sotd [OPTIONS]" ;; esac\n' "$v" > "$r/x/$top/sotd" ;;
        *) cat > "$r/x/$top/sotd" <<SD
#!/bin/sh
case "\$1 \$2" in
    "--version "*) echo "sotd $v (abc 2026-01-01, protocol 3)"; exit 0 ;;
    "install --help") echo "Usage: sotd install --pack <folder> [options]"; exit 0 ;;
esac
echo "sotd \$* [\$(ls "\$3" | tr '\n' ' ')]" >> "$d/net.log"
echo "\${STUB_LINE:-PASS: Ship of Tools $v at abc on this computer}"
exit "\${STUB_RC:-0}"
SD
        ;;
    esac
    chmod +x "$r/x/$top/sotd"
    tar -czf "$r/$top.tar.gz" -C "$r/x" "$top"
    printf '0123456789abcdef0123456789abcdef01234567\n' > "$r/COMMIT"
    ( cd "$r" && sha256_of "$top.tar.gz" COMMIT > SHA256SUMS )
}
# The bootstrap as a person runs it: its output in ENTRY_OUT, its exit code in ENTRY_RC. [VAR=value...] before the args.
entry_run() {  # <d> [VAR=value...] [install.sh args...]
    local d="$1"; shift
    local envs=()
    while [ $# -gt 0 ]; do case "$1" in *=*) envs+=("$1"); shift ;; *) break ;; esac; done
    ENTRY_RC=0
    ENTRY_OUT="$(env -i HOME="$d/home" PATH="$d/stubs:$ENTRY_TOOLS" TMPDIR="$d" SOT_INSTALL_REPO=o/r ${envs[@]+"${envs[@]}"} \
        bash "$ENTRY_SRC/scripts/install.sh" "$@" 2>&1)" || ENTRY_RC=$?
}
verdicts() { printf '%s\n' "$ENTRY_OUT" | grep -c -E '^(PASS|FAIL|ONE STEP LEFT): ' || true; }
last() { printf '%s\n' "$ENTRY_OUT" | grep -v '^[[:space:]]*$' | tail -1; }
logged() { grep -c -F -- "$2" "$1/net.log" || true; }
record() {  # <d> <version>: an install at the default prefix that records <version>
    mkdir -p "$1/home/.local/share/sot"
    printf '{\n  "schema": 1,\n  "version": "%s"\n}\n' "$2" > "$1/home/.local/share/sot/install.json"
}

# ---------------------------------------------------------------------------
case_start "entry_runs_the_tags_own_installer"
d="$WORK/e-channel"; entry_fixture "$d"; printf 'v2.0.0\n' > "$d/rel/latest"; printf 'v2.1.0-rc1\n' > "$d/rel/newest"
entry_run "$d" --local
check "no install: the channel is the latest stable release, run as an implicit run with the same arguments" "1 0 1" \
    "$(logged "$d" 'tag-installer v2.0.0 1 --local') $ENTRY_RC $(verdicts)"
record "$d" 1.9.0-rc2; : > "$d/net.log"; entry_run "$d"
check "a pre-release: the newest published release of any kind" "1" "$(logged "$d" 'tag-installer v2.1.0-rc1 1')"
record "$d" 2.0.1-dev+abc123; : > "$d/net.log"; entry_run "$d"
check "a build of the line: the newest published release too (its own sotd install says which way that moves it)" "1" "$(logged "$d" 'tag-installer v2.1.0-rc1 1')"
: > "$d/net.log"; entry_run "$d" --version v1.5.0 --be-only
check "a named version: that tag's installer, told it was named" "1 0" "$(logged "$d" 'tag-installer v1.5.0 0 --version v1.5.0 --be-only') $(logged "$d" 'api.github.com')"
check "every curl call carries the connect and stall bound" "0" \
    "$(grep '^curl ' "$d/net.log" | grep -vc -e '--connect-timeout 15 --speed-limit 1000 --speed-time 60' || true)"

# ---------------------------------------------------------------------------
case_start "entry_checks_the_pack_before_anything_of_it_runs"
d="$WORK/e-pack"; entry_fixture "$d"; entry_pack "$d" v3.0.0
entry_run "$d" SOT_INSTALL_TAG=v3.0.0 --be-only
check "a named release is downloaded, checked and its own sotd install runs with --pack and the arguments" "0 1" \
    "$ENTRY_RC $(grep -c 'sotd install --pack .* --be-only \[' "$d/net.log" || true)"
check "the pack it ran from holds SHA256SUMS, the archive, COMMIT and the unpacked folder" "[COMMIT SHA256SUMS sot-3.0.0-$TGT sot-3.0.0-$TGT.tar.gz ]" \
    "$(sed -n 's/^sotd install.*\(\[.*\]\)$/\1/p' "$d/net.log" | head -1)"
check "its scratch is gone" "0" "$(find "$d" -maxdepth 1 -name 'sot-install.*' | wc -l | tr -d ' ')"
mkdir -p "$d/arch"; cp "$d/rel/v3.0.0/"*.tar.gz "$d/rel/v3.0.0/COMMIT" "$d/rel/v3.0.0/SHA256SUMS" "$d/arch/"
: > "$d/net.log"; entry_run "$d" --archive "$d/arch" --be-only
check "--archive runs this bootstrap with the folder's pack and fetches nothing" "0 1 0" "$ENTRY_RC $(logged "$d" 'sotd install --pack') $(grep -c '^curl ' "$d/net.log" || true)"
printf 'tampered\n' >> "$d/arch/sot-3.0.0-$TGT.tar.gz"; : > "$d/net.log"; entry_run "$d" --archive "$d/arch"
check "an archive that does not match SHA256SUMS: FAIL before its sotd runs" "1 0 1" "$ENTRY_RC $(logged "$d" 'sotd install') $(verdicts)"
check "the line names the check" "FAIL: verify: checksum verification FAILED" "$(last)"
cp "$d/rel/v3.0.0/"*.tar.gz "$d/arch/"; ( cd "$d/arch" && sha256_of "sot-3.0.0-$TGT.tar.gz" > SHA256SUMS ); entry_run "$d" --archive "$d/arch"
check "a COMMIT SHA256SUMS does not list is refused" "FAIL: verify: COMMIT is not listed in SHA256SUMS; every file of the pack must be checksummed" "$(last)"
( cd "$d/arch" && sha256_of "sot-3.0.0-$TGT.tar.gz" COMMIT > SHA256SUMS; printf 'x\n' > LIBC ); entry_run "$d" --archive "$d/arch"
check "a LIBC SHA256SUMS does not list is refused" "FAIL: verify: LIBC is not listed in SHA256SUMS; every file of the pack must be checksummed" "$(last)"
v=0.6.7-dev+7a64a6060; entry_pack "$d" "v$v"; rm -rf "$d/arch"; mkdir -p "$d/arch"; cp "$d/rel/v$v/"*.tar.gz "$d/rel/v$v/COMMIT" "$d/rel/v$v/SHA256SUMS" "$d/arch/"
entry_run "$d" --archive "$d/arch"
check "a version with a + is a name, not a pattern" "0" "$ENTRY_RC"
entry_pack "$d" v3.0.1 norun; entry_run "$d" SOT_INSTALL_TAG=v3.0.1
check "an unpacked sotd that cannot run: its own error, then one FAIL" "1 1 1" "$ENTRY_RC $(printf '%s\n' "$ENTRY_OUT" | grep -c 'GLIBC_2.99 not found' || true) $(verdicts)"
entry_pack "$d" v3.0.2 old; entry_run "$d" SOT_INSTALL_TAG=v3.0.2
check "a release whose sotd has no install is installed only by its own installer" "1" "$(printf '%s\n' "$ENTRY_OUT" | grep -c 'release 3.0.2 predates sotd install' || true)"

# ---------------------------------------------------------------------------
case_start "entry_ends_every_exit_in_one_verdict_line"
d="$WORK/e-verdict"; entry_fixture "$d"; entry_pack "$d" v3.0.0
entry_run "$d" --version
check "an option with no value: one FAIL, exit 2" "2 1 FAIL: options: --version needs a value" "$ENTRY_RC $(verdicts) $(last)"
entry_run "$d" SOT_INSTALL_TAG=v3.0.0 STUB_LINE="FAIL: gate: refused" STUB_RC=2
check "sotd install's refusal is the run's one line and its exit code" "2 1 FAIL: gate: refused" "$ENTRY_RC $(verdicts) $(last)"
entry_run "$d" SOT_INSTALL_TAG=v3.0.0 STUB_LINE="partial output" STUB_RC=9
check "a sotd install that ends with no verdict is named" "1 1 FAIL: install: sotd install ended with exit 9 and no verdict" "$ENTRY_RC $(verdicts) $(last)"
entry_run "$d" SOT_INSTALL_TAG=v9.9.9
check "a release with no assets: one FAIL" "1 1" "$ENTRY_RC $(verdicts)"
printf '#!/bin/sh\nexit 6\n' > "$d/stubs/curl"; chmod +x "$d/stubs/curl"
entry_run "$d"
check "an unreachable network: one FAIL naming the target step" "1 1" "$ENTRY_RC $(verdicts)"
check "the line says what it could not do" "FAIL: target: could not resolve the latest release of o/r" "$(last)"

# ---------------------------------------------------------------------------
# The real sotd as the pack's: a refused install changes nothing under the home (the prefix, ~/.local, the comm home).
REAL_SOTD="${SOT_TEST_SOTD:-${CARGO_TARGET_DIR:-$ENTRY_SRC/rust/target}/debug/sotd}"
case_start "entry_refuses_without_writing"
real_help="$("$REAL_SOTD" install --help 2>/dev/null)" || real_help=""
if [ ! -x "$REAL_SOTD" ] || [ "${real_help#*Usage: sotd install}" = "$real_help" ]; then
    printf '    SKIP the real sotd refusals (no sotd with install at %s)\n' "$REAL_SOTD"
elif ! command -v jq >/dev/null 2>&1; then
    printf '    SKIP the real sotd refusals (no jq on this host, which sotd install requires before its gates)\n'
else
    line="$("$REAL_SOTD" --version)"; rv="$(printf '%s\n' "$line" | cut -d' ' -f2)"; rsha="$(printf '%s\n' "$line" | sed -E 's/^[^(]*\(([0-9a-f]+).*/\1/')"
    rcommit="$(git -C "$ENTRY_SRC" rev-parse "$rsha" 2>/dev/null)" || rcommit=""
    real_pack() {  # <d>: a pack whose sotd is the real one, with a sot that says the same build
        local top="sot-$rv-$TGT"
        mkdir -p "$1/pack/$top"; cp "$REAL_SOTD" "$1/pack/$top/sotd"
        printf '#!/bin/sh\necho "sot %s (%s 2026-01-01, protocol 3)"\n' "$rv" "$rsha" > "$1/pack/$top/sot"; chmod +x "$1/pack/$top/sot"
        printf 'archive\n' > "$1/pack/$top.tar.gz"; printf '%s\n' "$rcommit" > "$1/pack/COMMIT"
        ( cd "$1/pack" && sha256_of "$top.tar.gz" COMMIT > SHA256SUMS )
    }
    # An install a daemon host left on the shared home: its record, binaries and a pointer armed over it.
    shared_install() {  # <d>
        local p="$1/home/.local/share/sot"
        mkdir -p "$p/bin" "$p/updates" "$1/home/.config/sot" "$1/run"; chmod 700 "$1/run"
        printf '{\n  "schema": 1,\n  "version": "0.0.1",\n  "daemon": true,\n  "service": "systemd"\n}\n' > "$p/install.json"
        printf 'daemon host sotd\n' > "$p/bin/sotd"; printf '{"tag": "v9"}\n' > "$p/updates/pending-$TGT.json"
        printf 'hub = "hub-box"\n[host.hub-box]\ndaemon = true\n' > "$1/home/.config/sot/hosts.toml"
    }
    sotd_install() {  # <d> <PATH> <args...>: sotd install run as the bootstrap runs it
        local d="$1" path="$2"; shift 2
        ENTRY_RC=0
        ENTRY_OUT="$(env -i HOME="$d/home" PATH="$path" XDG_RUNTIME_DIR="$d/run" SOT_SELF_HOST=laptop "$d/pack/sot-$rv-$TGT/sotd" install --pack "$d/pack" "$@" 2>&1)" || ENTRY_RC=$?
    }
    if [ -z "$rcommit" ]; then
        printf '    SKIP the real sotd refusals (its commit %s is not in this tree)\n' "$rsha"
    else
        JQ_TOOLS="$WORK/entry-jq"; mk_tools "$JQ_TOOLS" jq
        d="$WORK/e-refuse-host"; mkdir -p "$d"; real_pack "$d"; shared_install "$d"; before="$(tree_of "$d/home")"
        sotd_install "$d" "$JQ_TOOLS" --be-only --no-service
        check "a host hosts.toml runs no sotd on is refused over the shared install (exit 2, one verdict)" "2 1" "$ENTRY_RC $(verdicts)"
        check "it says why" "1" "$(printf '%s\n' "$ENTRY_OUT" | grep -c 'FAIL: gate: hosts.toml runs no sotd on this host, and .* already holds an install' || true)"
        check "nothing under the home changed" "$before" "$(tree_of "$d/home")"
        # A backend-only run never starts the window: a frontend that cannot run on this host's libc does not stop it.
        printf '#!/bin/sh\necho "sot: GLIBC_2.35 not found" >&2\nexit 1\n' > "$d/pack/sot-$rv-$TGT/sot"
        sotd_install "$d" "$JQ_TOOLS" --be-only --no-service
        check "a --be-only run whose frontend cannot start here goes on to its gates" "2 1" \
            "$ENTRY_RC $(printf '%s\n' "$ENTRY_OUT" | grep -c 'FAIL: gate: hosts.toml runs no sotd on this host' || true)"
        d="$WORK/e-refuse-jq"; mkdir -p "$d"; real_pack "$d"; shared_install "$d"; before="$(tree_of "$d/home")"
        sotd_install "$d" "$ENTRY_TOOLS" --be-only --no-service
        check "an install without jq stops naming the command that installs it" "1 1" \
            "$ENTRY_RC $(printf '%s\n' "$ENTRY_OUT" | grep -c 'sot-comm needs jq, which is not on PATH: install it with ' || true)"
        check "nothing under the home changed" "$before" "$(tree_of "$d/home")"
        d="$WORK/e-refuse-flag"; mkdir -p "$d"; real_pack "$d"
        sotd_install "$d" "$JQ_TOOLS" --frob
        check "an unknown option: one FAIL, exit 2" "2 1 FAIL: options: unknown flag: --frob" "$ENTRY_RC $(verdicts) $(last)"
        sotd_install "$d" "$JQ_TOOLS" --prefix "$d/a b"
        check "a prefix with a space is refused before anything is written" "1 no" "$ENTRY_RC $([ -e "$d/a b" ] && echo yes || echo no)"
    fi
fi
