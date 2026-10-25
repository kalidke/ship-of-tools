#!/bin/bash
# Invoke actual development entries, stopping at recording tools or their existing container precondition.
. "${BASH_SOURCE[0]%/*}/publish-guard-fixture.sh"
ENTRY_ROOT=${PUBLISH_GUARD_ENTRY_ROOT:-$ROOT}
fixture entries
# Make the source fixture look like the minimal release/pack input, with recording build tools.
cp "$ENTRY_ROOT/scripts/release.sh" "$ENTRY_ROOT/scripts/pack-build.sh" "$ENTRY_ROOT/scripts/pack-build.ps1" "$ENTRY_ROOT/scripts/sot-install-layout.ps1" "$R/scripts/"
mkdir -p "$R/rust" "$T/tools"
printf '[workspace.package]\nversion = "0.6.6-dev"\n' > "$R/rust/Cargo.toml"
printf 'version = 3\n' > "$R/rust/Cargo.lock"
printf 'name = "Fixture"\nversion = "0.6.6-dev"\n' > "$R/Project.toml"
commit
accept 'development fixture prepared' origin main
# Removing the test-owned shim simulates a never-enrolled clone while keeping its explicit synthetic list.
unenroll() { rm -f "$R/.git/hooks/pre-push"; }
check_enrolled() {
    reject "$1 enrolled before stopping" 'update 1 (object' origin HEAD:refs/heads/glimmerdeny
}
cat > "$T/tools/cargo" <<'CARGO'
#!/bin/sh
printf '%s\n' "$*" >> "$HOME/build-calls"
case "$1" in run|update) exit 0 ;; esac
exit 73
CARGO
fixture_executable "$T/tools/cargo"
# The release performs a real atomic push; a synthetic denied tag cannot reach the local destination.
# Its fake comparator/lock updater are recording tools; no actual Rust build runs.
unenroll
before=$(refs)
if PATH="$T/tools:$PATH" "$BASH" "$R/scripts/release.sh" 0.6.6-glimmerdeny --yes > "$T/entry.out" 2>&1; then fail 'denied release cut accepted'; fi
[ "$before" = "$(refs)" ] || fail 'denied release cut published refs'
grep -F 'pre-push: tag update' "$T/entry.out" > /dev/null || fail 'release did not reach guarded push'
[ -s "$HOME/build-calls" ] || fail 'release comparator was not reached'
ok 'release entry activates guard before actual publication'
check_enrolled release
# Restore the scratch fixture after the deliberately refused cut (release commits remain local by design).
git -C "$R" reset -q --hard origin/main
git -C "$R" tag -d v0.6.6-glimmerdeny > /dev/null
case "$(uname -s)" in
    Linux)
        unenroll
        if CARGO_TARGET_DIR="$T/gate-target" bash "$ENTRY_ROOT/scripts/tests/rc-gate.sh" "$R" "$T/gate-logs" > "$T/entry.out" 2>&1; then fail 'gate unexpectedly passed outside its container'; fi
        grep -E 'rc-gate: (needs a cgroup|run it)' "$T/entry.out" > /dev/null || fail 'gate did not reach known container boundary'
        check_enrolled 'gate checkout argument'
        # The pack must pass platform/libc checks before the controlled build stop.
        printf '#!/bin/sh\nexit 0\n' > "$T/tools/musl-gcc"; fixture_executable "$T/tools/musl-gcc"
        rm -f "$HOME/build-calls"
        unenroll
        if PATH="$T/tools:$PATH" "$BASH" "$R/scripts/pack-build.sh" "$T/pack-out" > "$T/entry.out" 2>&1; then fail 'pack passed controlled build stop'; fi
        grep -F 'build --release' "$HOME/build-calls" > /dev/null || fail 'pack build boundary not reached'
        check_enrolled 'shell pack'
        ;;
    MINGW*|MSYS*|CYGWIN*)
        skip 'gate checkout argument enrollment: the gate requires Linux systemd cgroups'
        skip 'shell pack enrollment: pack-build.sh is Linux-only; Windows exercises pack-build.ps1'
        # The real Windows PowerShell entry discovers Git for Windows bash itself.
        printf '@echo off\r\necho build-called>"%%USERPROFILE%%\\build-called"\r\nexit /b 73\r\n' > "$T/tools/cargo.cmd"
        unenroll
        if USERPROFILE="$(cygpath -aw "$HOME")" PATH="$T/tools:$PATH" powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$(cygpath -aw "$R/scripts/pack-build.ps1")" -Out "$(cygpath -aw "$T/pack-out")" > "$T/entry.out" 2>&1; then fail 'PowerShell pack passed controlled build stop'; fi
        [ -f "$HOME/build-called" ] || fail 'PowerShell pack build boundary not reached'
        grep -F 'cargo build failed (73)' "$T/entry.out" > /dev/null || fail 'PowerShell pack wrong stop'
        check_enrolled 'PowerShell pack'
        ;;
esac
# Conflicts stop each entry before its substantive tool calls.
git -C "$R" config core.hooksPath "$T/foreign"
rm -f "$HOME/build-calls"
if PATH="$T/tools:$PATH" "$BASH" "$R/scripts/release.sh" 0.6.6 --yes > "$T/entry.out" 2>&1; then fail 'release ignored hook conflict'; fi
[ ! -e "$HOME/build-calls" ] || fail 'release built before conflict refusal'
grep -F 'existing hooks configuration kept' "$T/entry.out" > /dev/null || fail 'release conflict wrong cause'
ok 'release conflict stops before build'
case "$(uname -s)" in Linux)
    if PATH="$T/tools:$PATH" "$BASH" "$R/scripts/pack-build.sh" "$T/conflict-out" > "$T/entry.out" 2>&1; then fail 'pack ignored hook conflict'; fi
    [ ! -e "$HOME/build-calls" ] || fail 'pack built before conflict refusal'
    grep -F 'existing hooks configuration kept' "$T/entry.out" > /dev/null || fail 'pack conflict wrong cause'
    ok 'pack conflict stops before build'
    ;;
    MINGW*|MSYS*|CYGWIN*) skip 'shell pack conflict before build: pack-build.sh is Linux-only' ;;
esac
printf '1..%s\n' "$count"
