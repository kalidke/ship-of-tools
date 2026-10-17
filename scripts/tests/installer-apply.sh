#!/usr/bin/env bash
# installer-apply.sh -- the sot-apply transaction, the one-copy helper, the network refusal and the dev launcher's git bound.
# Run: scripts/tests/installer-apply.sh

set -euo pipefail

# shellcheck source=installer-support.sh
. "$(dirname "$0")/installer-support.sh"

# ---------------------------------------------------------------------------
# sot-apply fixtures: an install prefix with a real sot-apply, a git checkout
# holding this tree's library and unit template, and a staged v9.9.9.
SRC="$(cd "$(dirname "$0")/../.." && pwd)"
mk_apply_fixture() {  # <dir> <owned-unit 0|1> <owned-wrapper 0|1>
    local d="$1" ou="$2" ow="$3" h="$1/home" P="$1/prefix" co="$1/co" other=/other/prefix
    local stage="$1/prefix/updates/v9.9.9-$TGT" top="sot-v9.9.9-$TGT" sha unit_owner wrap_owner
    mkdir -p "$h/.config/systemd/user" "$h/.local/bin" "$P/bin" "$P/updates" "$P/repo" "$d/prev-co" \
        "$co/scripts/lib" "$co/deploy" "$stage/$top" "$d/stubs"
    mk_stubs "$d/stubs"
    : > "$d/log"
    cp "$SRC/scripts/sot-apply.sh" "$P/bin/sot-apply"; chmod 0755 "$P/bin/sot-apply"
    printf 'old-sot\n' > "$P/bin/sot"; printf 'old-sotd\n' > "$P/bin/sotd"
    chmod 0755 "$P/bin/sot" "$P/bin/sotd"
    printf '{\n  "tag": "v9.9.8",\n  "version": "9.9.8",\n  "commit": "aaa",\n  "service": "systemd"\n}\n' > "$P/install.json"
    ln -sfn "$d/prev-co" "$P/repo/current"
    cp "$SRC/scripts/lib/sot-daemon.sh" "$co/scripts/lib/"; cp "$SRC/deploy/sotd.service" "$co/deploy/"
    ( cd "$co" && git init -q . && git add . && git -c user.name=t -c user.email=t@t commit -q -m c )
    printf 'new-sot\n' > "$stage/$top/sot"; printf 'new-sotd\n' > "$stage/$top/sotd"
    printf 'asset\n' > "$stage/$top.tar.gz"
    sha="$(sha256_of "$stage/$top.tar.gz" | cut -d' ' -f1)"
    printf '{}\n' > "$stage/manifest.json"
    printf '{\n  "tag": "v9.9.9",\n  "target": "'"$TGT"'",\n  "checkout": "%s",\n  "commit": "%s",\n  "asset": "%s.tar.gz",\n  "asset_sha256": "%s"\n}\n' \
        "$co" "$(cd "$co" && git rev-parse HEAD)" "$top" "$sha" > "$P/updates/pending-$TGT.json"
    unit_owner="$other"; [ "$ou" = 1 ] && unit_owner="$P"
    wrap_owner="$other"; [ "$ow" = 1 ] && wrap_owner="$P"
    printf '[Service]\nExecStart=%s/bin/sotd --x\nRestart=always\n' "$unit_owner" > "$h/.config/systemd/user/sotd.service"
    printf '#!/usr/bin/env bash\nPENDING="%s/updates/pending-'"$TGT"'.json"\nstart_daemon_if_needed() {\n:\n}\n' "$wrap_owner" > "$h/.local/bin/sot-launch"
    chmod 0755 "$h/.local/bin/sot-launch"
    cp "$h/.config/systemd/user/sotd.service" "$d/unit.orig"; cp "$h/.local/bin/sot-launch" "$d/wrap.orig"
}
run_apply() {  # <dir> [args]: sets AP_RC; rmdir is not in $TOOLS, so the staging lock is cleared here
    local d="$1"; shift
    rm -rf "${d:?}/prefix/updates/.lock"
    AP_RC=0
    ( HOME="$d/home" PATH="$d/stubs:$TOOLS" STUB_LOG="$d/log" STUB_SOCKET="$d/sot.sock" \
        "$d/prefix/bin/sot-apply" "$@" ) > "$d/out" 2>&1 || AP_RC=$?
}
reloads() { grep -c '^--user daemon-reload$' "$1/log" || true; }

# ---------------------------------------------------------------------------
case_start "apply_rerenders_owned_unit"
d="$WORK/ap1"; mk_apply_fixture "$d" 1 1; run_apply "$d"
U="$d/home/.config/systemd/user/sotd.service"
check "the applied tag is recorded" "1" "$(grep -c '"tag": "v9.9.9"' "$d/prefix/install.json" || true)"
check "the owned unit now restarts on failure" "1" "$(grep -c '^Restart=on-failure$' "$U" || true)"
linux_check "exactly one daemon-reload" "1" "$(reloads "$d")"
check "the unit backup equals the original" "same" "$(cmp -s "$d/unit.orig" "$d/prefix/updates/sotd.service.prev-$TGT" && echo same || echo differ)"

# ---------------------------------------------------------------------------
case_start "apply_skips_foreign_unit"
d="$WORK/ap2"; mk_apply_fixture "$d" 0 0; run_apply "$d"
check "the applied tag is recorded" "1" "$(grep -c '"tag": "v9.9.9"' "$d/prefix/install.json" || true)"
check "a foreign unit is byte-identical" "same" "$(cmp -s "$d/unit.orig" "$d/home/.config/systemd/user/sotd.service" && echo same || echo differ)"
check "no daemon-reload" "0" "$(reloads "$d")"
check "no unit backup" "no" "$([ -e "$d/prefix/updates/sotd.service.prev-$TGT" ] && echo yes || echo no)"

# ---------------------------------------------------------------------------
case_start "apply_rerenders_owned_wrapper"
d="$WORK/ap3"; mk_apply_fixture "$d" 1 1
W="$d/home/.local/bin/sot-launch"; ino0="$(ls -i "$W" | awk '{print $1}')"
run_apply "$d"
check "the wrapper carries the marker" "1" "$(grep -c '^# sot-launch: all-in-one$' "$W" || true)"
check "the wrapper is executable" "yes" "$([ -x "$W" ] && echo yes || echo no)"
check "the wrapper is a new inode" "changed" "$([ "$(ls -i "$W" | awk '{print $1}')" != "$ino0" ] && echo changed || echo same)"
check "the wrapper backup equals the original" "same" "$(cmp -s "$d/wrap.orig" "$d/prefix/updates/sot-launch.prev-$TGT" && echo same || echo differ)"

# ---------------------------------------------------------------------------
case_start "apply_skips_foreign_wrapper"
d="$WORK/ap4"; mk_apply_fixture "$d" 1 0; run_apply "$d"
check "the applied tag is recorded" "1" "$(grep -c '"tag": "v9.9.9"' "$d/prefix/install.json" || true)"
check "a foreign wrapper is byte-identical" "same" "$(cmp -s "$d/wrap.orig" "$d/home/.local/bin/sot-launch" && echo same || echo differ)"

# ---------------------------------------------------------------------------
case_start "apply_failure_restores_unit_and_wrapper"
d="$WORK/ap5"; mk_apply_fixture "$d" 1 1
chmod 0555 "$d/home/.local/bin"
run_apply "$d"
chmod 0755 "$d/home/.local/bin"
check "the unit is byte-equal to the original" "same" "$(cmp -s "$d/unit.orig" "$d/home/.config/systemd/user/sotd.service" && echo same || echo differ)"
linux_check "two daemon-reloads (re-render, restore)" "2" "$(reloads "$d")"
check "sotd is back to its pre-apply content" "old-sotd" "$(cat "$d/prefix/bin/sotd")"
check "repo/current points at the previous checkout" "$d/prev-co" "$(readlink "$d/prefix/repo/current")"
check "the pending pointer is still armed" "yes" "$([ -f "$d/prefix/updates/pending-$TGT.json" ] && echo yes || echo no)"
check "the wrapper is byte-equal to the original" "same" "$(cmp -s "$d/wrap.orig" "$d/home/.local/bin/sot-launch" && echo same || echo differ)"

# ---------------------------------------------------------------------------
case_start "rollback_restores_unit_and_wrapper"
d="$WORK/ap6"; mk_apply_fixture "$d" 1 1; run_apply "$d"
check "the apply re-rendered the unit" "1" "$(grep -c '^Restart=on-failure$' "$d/home/.config/systemd/user/sotd.service" || true)"
run_apply "$d" --rollback
check "the unit is byte-equal to the original" "same" "$(cmp -s "$d/unit.orig" "$d/home/.config/systemd/user/sotd.service" && echo same || echo differ)"
check "the wrapper is byte-equal to the original" "same" "$(cmp -s "$d/wrap.orig" "$d/home/.local/bin/sot-launch" && echo same || echo differ)"
check "the wrapper is executable" "yes" "$([ -x "$d/home/.local/bin/sot-launch" ] && echo yes || echo no)"
linux_check "a daemon-reload followed the restore" "2" "$(reloads "$d")"
check "the checkout flipped back" "$d/prev-co" "$(readlink "$d/prefix/repo/current")"

# ---------------------------------------------------------------------------
case_start "failed_apply_keeps_old_record_and_pending"
d="$WORK/ap7"; mk_apply_fixture "$d" 1 1
chmod 0555 "$d/home/.local/bin"
run_apply "$d"
chmod 0755 "$d/home/.local/bin"
check "install.json still names the old tag" "1" "$(grep -c '"tag": "v9.9.8"' "$d/prefix/install.json" || true)"
check "install.json still names the old commit" "1" "$(grep -c '"commit": "aaa"' "$d/prefix/install.json" || true)"
check "the pending pointer is still armed" "yes" "$([ -f "$d/prefix/updates/pending-$TGT.json" ] && echo yes || echo no)"
run_apply "$d"
check "a second apply installs the new tag" "1 new-sotd no" \
    "$(grep -c '"tag": "v9.9.9"' "$d/prefix/install.json" || true) $(cat "$d/prefix/bin/sotd") $([ -f "$d/prefix/updates/pending-$TGT.json" ] && echo yes || echo no)"

# ---------------------------------------------------------------------------
case_start "partial_wrapper_write_restores"
d="$WORK/ap8"; mk_apply_fixture "$d" 1 1
cat > "$d/stubs/cat" <<'CAT'
#!/bin/sh
# A disk that fills mid-write: half the text lands, then the write fails.
in="$(head -c 1000000)"
printf '%s' "$in" | head -c "$((${#in} / 2))"
exit 1
CAT
chmod +x "$d/stubs/cat"
run_apply "$d"
check "the apply fails at the re-render and restores" "1" "$(grep -c 're-rendering the unit or wrapper failed .* restoring previous binaries' "$d/out" || true)"
check "the old wrapper is byte-identical" "same" "$(cmp -s "$d/wrap.orig" "$d/home/.local/bin/sot-launch" && echo same || echo differ)"
check "the pending pointer is kept" "yes" "$([ -f "$d/prefix/updates/pending-$TGT.json" ] && echo yes || echo no)"
check "no temp file is left beside the wrapper" "0" "$(find "$d/home/.local/bin" -name 'sot-launch.new*' | wc -l | tr -d ' ')"

# ---------------------------------------------------------------------------
# A disk that fills during a backup copy: half the bytes land, then cp fails.
# Every other copy is the real cp.
half_cp_stub() {  # <dir> <destination glob>
    cat > "$1/stubs/cp" <<CP
#!/bin/sh
for a; do src="\$dst"; dst="\$a"; done
case "\$dst" in
    $2) head -c "\$((\$(wc -c < "\$src") / 2))" "\$src" > "\$dst"; exit 1 ;;
esac
exec "$TOOLS/cp" "\$@"
CP
    chmod +x "$1/stubs/cp"
}
for which in unit wrapper; do
    case_start "partial_${which}_backup_keeps_the_originals"
    d="$WORK/ap9-$which"; mk_apply_fixture "$d" 1 1
    case "$which" in
        unit) half_cp_stub "$d" '*/sotd.service.prev-*' ;;
        wrapper) half_cp_stub "$d" '*/sot-launch.prev-*' ;;
    esac
    run_apply "$d"
    check "$which: the apply fails at the backup" "1" "$(grep -c 'backing up the unit or wrapper failed' "$d/out" || true)"
    check "$which: the unit is byte-identical" "same" "$(cmp -s "$d/unit.orig" "$d/home/.config/systemd/user/sotd.service" && echo same || echo differ)"
    check "$which: the wrapper is byte-identical" "same" "$(cmp -s "$d/wrap.orig" "$d/home/.local/bin/sot-launch" && echo same || echo differ)"
    check "$which: no partial backup is left" "0" "$(find "$d/prefix" "$d/home" -name '*.new*' | wc -l | tr -d ' ')"
done

# ---------------------------------------------------------------------------
for k in sot sotd; do
    case_start "failed_${k}_install_restores_only_this_apply"
    # Backups left by an older apply, then installing $k fails after every
    # binary before it was replaced.
    d="$WORK/ap10-$k"; mk_apply_fixture "$d" 1 1
    printf 'older-sot\n' > "$d/prefix/bin/sot.prev"; printf 'older-sotd\n' > "$d/prefix/bin/sotd.prev"
    cp "$d/prefix/bin/sot-apply" "$d/apply.orig"
    cat > "$d/stubs/install" <<INST
#!/bin/sh
for a; do dst="\$a"; done
case "\$dst" in */bin/$k.new) exit 1 ;; esac
exec "$TOOLS/install" "\$@"
INST
    chmod +x "$d/stubs/install"
    run_apply "$d"
    check "$k: the apply fails installing $k" "1" "$(grep -c "installing $k failed" "$d/out" || true)"
    check "$k: every binary has its pre-apply bytes" "old-sot old-sotd same" \
        "$(cat "$d/prefix/bin/sot") $(cat "$d/prefix/bin/sotd") $(cmp -s "$d/apply.orig" "$d/prefix/bin/sot-apply" && echo same || echo differ)"
    check "$k: install.json still names the old version" "1" "$(grep -c '"version": "9.9.8"' "$d/prefix/install.json" || true)"
done

# ---------------------------------------------------------------------------
case_start "backup_failure_replaces_nothing"
# An older apply's backups, then the second binary's backup fails half-written.
d="$WORK/ap11"; mk_apply_fixture "$d" 1 1
printf 'older-sot\n' > "$d/prefix/bin/sot.prev"; printf 'older-sotd\n' > "$d/prefix/bin/sotd.prev"
printf 'older-unit\n' > "$d/prefix/updates/sotd.service.prev-$TGT"
printf 'older-wrap\n' > "$d/prefix/updates/sot-launch.prev-$TGT"
cp "$d/prefix/bin/sot-apply" "$d/apply.orig"
half_cp_stub "$d" '*/bin/sotd.prev*'
run_apply "$d"
check "the apply exits non-zero" "yes" "$([ "$AP_RC" -ne 0 ] && echo yes || echo no)"
check "the failed backup is named" "1" "$(grep -c 'backing up sotd failed' "$d/out" || true)"
check "no installed file changed" "old-sot old-sotd same same same" \
    "$(cat "$d/prefix/bin/sot") $(cat "$d/prefix/bin/sotd") $(cmp -s "$d/apply.orig" "$d/prefix/bin/sot-apply" && echo same || echo differ) $(cmp -s "$d/unit.orig" "$d/home/.config/systemd/user/sotd.service" && echo same || echo differ) $(cmp -s "$d/wrap.orig" "$d/home/.local/bin/sot-launch" && echo same || echo differ)"
check "the old backup set is intact" "older-sot older-sotd older-unit older-wrap" \
    "$(cat "$d/prefix/bin/sot.prev") $(cat "$d/prefix/bin/sotd.prev") $(cat "$d/prefix/updates/sotd.service.prev-$TGT") $(cat "$d/prefix/updates/sot-launch.prev-$TGT")"
check "no .new is left" "0" "$(find "$d/prefix" "$d/home" -name '*.new*' | wc -l | tr -d ' ')"
check "repo/current and install.json are unchanged" "$d/prev-co 1" \
    "$(readlink "$d/prefix/repo/current") $(grep -c '"tag": "v9.9.8"' "$d/prefix/install.json" || true)"

# ---------------------------------------------------------------------------
case_start "rollback_copy_failure_fails_the_rollback"
d="$WORK/ap12"; mk_apply_fixture "$d" 1 1; run_apply "$d"
check "the apply installed the new tag" "1" "$(grep -c '"tag": "v9.9.9"' "$d/prefix/install.json" || true)"
cp "$d/prefix/install.json" "$d/record.applied"
half_cp_stub "$d" '*/bin/sotd|*/bin/sotd.new.*'
run_apply "$d" --rollback
check "the rollback exits non-zero" "yes" "$([ "$AP_RC" -ne 0 ] && echo yes || echo no)"
check "the error names the binary" "1" "$(grep -cF "could not restore $d/prefix/bin/sotd " "$d/out" || true)"
check "install.json is unchanged" "same" "$(cmp -s "$d/record.applied" "$d/prefix/install.json" && echo same || echo differ)"
check "repo/current is unchanged" "$d/co" "$(readlink "$d/prefix/repo/current")"
check "no rollback-complete line" "0" "$(grep -c 'rollback complete' "$d/out" || true)"

# ---------------------------------------------------------------------------
case_start "one_copy_helper"
helper_text() { sed -n '/^sot_install_copy() {/,/^}/p' "$1"; }
check "the library defines sot_install_copy" "yes" "$([ -n "$(helper_text "$LIB")" ] && echo yes || echo no)"
for f in "$SRC/scripts/sot-apply.sh" "$SRC/scripts/install.sh"; do
    check "$(basename "$f")'s sot_install_copy is byte-identical to the library's" "$(helper_text "$LIB")" "$(helper_text "$f")"
done
check "no other script defines it" "3" "$(grep -rl '^sot_install_copy() {' "$SRC/scripts" | wc -l | tr -d ' ')"
# A stale <dst>.new another writer left (or still writes) is never reused.
d="$WORK/copy-stale"; mkdir -p "$d"
printf 'source\n' > "$d/src"; printf 'old\n' > "$d/dst"; printf 'junk\n' > "$d/dst.new"
sot_install_copy "$d/src" "$d/dst"
check "the destination equals the source" "source" "$(cat "$d/dst")"
check "a stale dst.new is untouched" "junk" "$(cat "$d/dst.new" 2>/dev/null || echo gone)"
check "no temp file of this copy is left" "0" "$(find "$d" -name 'dst.new.*' | wc -l | tr -d ' ')"

# ---------------------------------------------------------------------------
case_start "remote_wrapper_failed_write_keeps_the_old"
# The --backend wrapper: a cat that fails before writing a byte.
d="$WORK/remote-wrap"; mkdir -p "$d/stubs"
printf '#!/bin/sh\necho old-wrapper\n' > "$d/sot-launch"; cp "$d/sot-launch" "$d/wrap.orig"
printf '#!/bin/sh\nexit 1\n' > "$d/stubs/cat"; chmod +x "$d/stubs/cat"
rc=0; ( PATH="$d/stubs:$PATH"; installer_render_remote_launch /opt/sot be-alias "$d/sot-launch" ) 2>/dev/null || rc=$?
check "the failed write returns 1" "1" "$rc"
check "the old remote wrapper is byte-identical" "same" "$(cmp -s "$d/wrap.orig" "$d/sot-launch" && echo same || echo differ)"
check "no temp file is left beside it" "0" "$(find "$d" -name 'sot-launch.new*' | wc -l | tr -d ' ')"
installer_render_remote_launch /opt/sot be-alias "$d/sot-launch"
check "a good write names the alias and is executable" "1 yes" \
    "$(grep -c '^export SOT_HOST="be-alias"$' "$d/sot-launch" || true) $([ -x "$d/sot-launch" ] && echo yes || echo no)"

# ---------------------------------------------------------------------------
case_start "a pinned run against a release older than scripts/lib/sot-daemon.sh refuses first"
# curl is stubbed: the library URL is a 404 (an old tree), anything else is
# logged and fails. Nothing may be created under the prefix.
d="$WORK/old-tree"; mkdir -p "$d/stubs" "$d/home"
cat > "$d/stubs/curl" <<'STUBEOF'
#!/bin/sh
echo "$*" >> "$CURL_LOG"
case "$*" in *scripts/lib/sot-daemon.sh*) exit 22 ;; esac
exit 22
STUBEOF
chmod +x "$d/stubs/curl"
rc=0
out="$(env -i HOME="$d/home" PATH="$d/stubs:/usr/bin:/bin" CURL_LOG="$d/curl.log" SOT_INSTALL_TAG=v0.0.1 \
    bash "$(dirname "$0")/../install.sh" --local --no-service --prefix "$d/prefix" 2>&1)" || rc=$?
check "the install exits non-zero" "yes" "$([ "$rc" -ne 0 ] && echo yes || echo no)"
check "it says the release predates this installer" "1" \
    "$(printf '%s\n' "$out" | grep -c 'release v0.0.1 predates this installer (no scripts/lib/sot-daemon.sh)' || true)"
check "the prefix was never created" "gone" "$([ -e "$d/prefix" ] && echo exists || echo gone)"
check "no release asset was requested" "0" "$(grep -c 'releases/download' "$d/curl.log" || true)"
# A network failure (curl exit 6) is said as one, never as an old release.
sed -i.bak 's/exit 22 ;; esac/exit 6 ;; esac/' "$d/stubs/curl"
rc=0
out="$(env -i HOME="$d/home" PATH="$d/stubs:/usr/bin:/bin" CURL_LOG="$d/curl.log" SOT_INSTALL_TAG=v0.0.1 \
    bash "$(dirname "$0")/../install.sh" --local --no-service --prefix "$d/prefix" 2>&1)" || rc=$?
check "an unreachable network exits non-zero" "yes" "$([ "$rc" -ne 0 ] && echo yes || echo no)"
check "it names the network, not the release" "1 0" \
    "$(printf '%s\n' "$out" | grep -c 'cannot reach raw.githubusercontent.com to check release v0.0.1 (curl exit 6)' || true) $(printf '%s\n' "$out" | grep -c 'predates' || true)"
check "the prefix was still never created" "gone" "$([ -e "$d/prefix" ] && echo exists || echo gone)"

# ---------------------------------------------------------------------------
case_start "the dev launcher's git pull carries the stall bound"
# A stand-in git records its argv and answers the pull with success; the launcher then goes on, finds no sotd, and
# execs the stub frontend. The pull must not be able to hold the window back on a stalled network.
d="$WORK/launch-pull"; mkdir -p "$d/repo/scripts/lib" "$d/repo/.git" "$d/home" "$d/stubs"
cp "$(dirname "$0")/../launch-sot.sh" "$d/repo/scripts/launch-sot.sh"
cp "$(dirname "$0")/../lib/sot-hosts.sh" "$(dirname "$0")/../lib/sot-daemon.sh" "$d/repo/scripts/lib/"
printf '#!/bin/sh\necho "git $*" >> "%s/git.log"\n' "$d" > "$d/stubs/git"
printf '#!/bin/sh\necho fe >> "%s/git.log"\n' "$d" > "$d/fe"
chmod +x "$d/stubs/git" "$d/fe"
( HOME="$d/home" PATH="$d/stubs:$TOOLS" SOT_PREFIX= SOT_FRONTEND_BIN="$d/fe" bash "$d/repo/scripts/launch-sot.sh" ) >/dev/null 2>&1 || true
check "the launcher pulled, then started the frontend" "1 1" "$(grep -c ' pull ' "$d/git.log" || true) $(grep -c '^fe$' "$d/git.log" || true)"
check "the pull carries the stall bound" "0" \
    "$(grep ' pull ' "$d/git.log" | grep -vc -e '-c http.lowSpeedLimit=1000 -c http.lowSpeedTime=60' || true)"

# ---------------------------------------------------------------------------
# --archive installs a folder pack-build.sh packed. HOME, the prefix and the folder are temp; SOT_INSTALL_TAG=x keeps
# the prelude from reaching the network.
mk_archive() {  # <dir> <digest-of-the-tarball 0|1> <with-COMMIT 0|1>: <dir>/arch holds sot-1.2.3-dev-$TGT.tar.gz
    local d="$1" a="$1/arch"
    mkdir -p "$a" "$d/home"
    printf 'not a real tarball\n' > "$a/sot-1.2.3-dev-$TGT.tar.gz"
    if [ "$2" = 1 ]; then
        ( cd "$a" && sha256_of "sot-1.2.3-dev-$TGT.tar.gz" > SHA256SUMS )
    else
        printf '%064d  sot-1.2.3-dev-%s.tar.gz\n' 0 "$TGT" > "$a/SHA256SUMS"
    fi
    [ "$3" = 0 ] || printf '%s\n' 0123456789abcdef0123456789abcdef01234567 > "$a/COMMIT"
    [ "$3" = 0 ] || ( cd "$a" && sha256_of COMMIT >> SHA256SUMS )
}
run_archive() {  # <dir>: sets AR_RC and AR_OUT
    local d="$1"
    AR_RC=0
    AR_OUT="$(env -i HOME="$d/home" PATH=/usr/bin:/bin SOT_INSTALL_TAG=x \
        bash "$(dirname "$0")/../install.sh" --be-only --no-service --prefix "$d/prefix" --archive "$d/arch" 2>&1)" || AR_RC=$?
}

# ---------------------------------------------------------------------------
case_start "archive_install_verifies_before_writing"
d="$WORK/ar-sum"; mk_archive "$d" 0 1; run_archive "$d"
check "the install exits non-zero" "yes" "$([ "$AR_RC" -ne 0 ] && echo yes || echo no)"
check "it says the checksum failed" "1" "$(printf '%s\n' "$AR_OUT" | grep -c 'checksum verification FAILED' || true)"
check "the prefix was never created" "gone" "$([ -e "$d/prefix" ] && echo exists || echo gone)"

# ---------------------------------------------------------------------------
case_start "archive_install_needs_its_commit"
d="$WORK/ar-commit"; mk_archive "$d" 1 0; run_archive "$d"
check "the install exits non-zero" "yes" "$([ "$AR_RC" -ne 0 ] && echo yes || echo no)"
check "it names COMMIT" "1" "$(printf '%s\n' "$AR_OUT" | grep -c 'COMMIT' || true)"
check "the prefix was never created" "gone" "$([ -e "$d/prefix" ] && echo exists || echo gone)"

# ---------------------------------------------------------------------------
# A real tarball: sot and sotd are stubs that print <sotd-line> / <sot-line> for --version and answer nothing else.
# <dir>/prefix holds an old sotd to prove it survives a refusal (--force-role-change lets the gates pass over it).
# <dir>/origin is an empty bare repository the installer's base clone reaches through insteadOf (no network).
mk_packed() {  # <dir> <sotd-version> <sotd-sha> <sot-version>: <dir>/arch is sot-1.2.3-dev, COMMIT 0123456789abcdef...
    local d="$1" a="$1/arch" top="sot-1.2.3-dev-$TGT"
    mkdir -p "$a" "$d/home" "$d/pack/$top" "$d/prefix/bin"
    printf '#!/bin/sh\n[ "$1" = --version ] && echo "sotd %s (%s 2026-10-16, protocol 1)"\nexit 0\n' "$2" "$3" > "$d/pack/$top/sotd"
    printf '#!/bin/sh\n[ "$1" = --version ] && echo "sot %s (%s 2026-10-16, protocol 1)"\nexit 0\n' "$4" "$3" > "$d/pack/$top/sot"
    chmod 0755 "$d/pack/$top/sotd" "$d/pack/$top/sot"
    printf 'old\n' > "$d/prefix/bin/sotd"
    tar -czf "$a/$top.tar.gz" -C "$d/pack" "$top"
    printf '%s\n' 0123456789abcdef0123456789abcdef01234567 > "$a/COMMIT"
    ( cd "$a" && sha256_of "$top.tar.gz" COMMIT > SHA256SUMS )
    git init -q --bare "$d/origin"
    printf '[url "%s/"]\n\tinsteadOf = https://github.com/kalidke/ship-of-tools\n' "$d/origin" > "$d/gitconfig"
}
run_packed() {  # <dir>: like run_archive, with the git redirect and a PATH that reaches jq and git
    local d="$1"
    AR_RC=0
    AR_OUT="$(env -i HOME="$d/home" PATH=/usr/bin:/bin SOT_INSTALL_TAG=x GIT_CONFIG_GLOBAL="$d/gitconfig" \
        bash "$(dirname "$0")/../install.sh" --be-only --no-service --force-role-change --prefix "$d/prefix" --archive "$d/arch" 2>&1)" || AR_RC=$?
}

# ---------------------------------------------------------------------------
case_start "archive_install_binds_its_binaries_to_the_folder"
d="$WORK/ar-ver"; mk_packed "$d" 0.9.9 012345678 1.2.3-dev; run_packed "$d"
check "a sotd of another version is refused" "yes" "$([ "$AR_RC" -ne 0 ] && echo yes || echo no)"
check "it names the mismatch" "1" "$(printf '%s\n' "$AR_OUT" | grep -c 'does not match' || true)"
check "the installed sotd is the old one" "old" "$(cat "$d/prefix/bin/sotd")"
d="$WORK/ar-sha"; mk_packed "$d" 1.2.3-dev fffffffff 1.2.3-dev; run_packed "$d"
check "a sotd built at another commit is refused" "1" "$(printf '%s\n' "$AR_OUT" | grep -c 'does not match' || true)"
check "the installed sotd is the old one" "old" "$(cat "$d/prefix/bin/sotd")"
d="$WORK/ar-sot"; mk_packed "$d" 1.2.3-dev 012345678 0.9.9; run_packed "$d"
check "a sot of another version is refused" "1" "$(printf '%s\n' "$AR_OUT" | grep -c 'does not match' || true)"
d="$WORK/ar-commit-sum"; mk_packed "$d" 1.2.3-dev 012345678 1.2.3-dev
printf '%s\n' ffffffffffffffffffffffffffffffffffffffff > "$d/arch/COMMIT"; run_packed "$d"
check "a COMMIT its SHA256SUMS does not cover is refused" "1" "$(printf '%s\n' "$AR_OUT" | grep -c 'checksum verification FAILED' || true)"

# ---------------------------------------------------------------------------
case_start "archive_install_refuses_an_absent_commit_before_swapping"
d="$WORK/ar-absent"; mk_packed "$d" 1.2.3-dev 012345678 1.2.3-dev; run_packed "$d"
check "the install exits non-zero" "yes" "$([ "$AR_RC" -ne 0 ] && echo yes || echo no)"
check "it says the commit is not in origin" "1" "$(printf '%s\n' "$AR_OUT" | grep -c 'is not in origin' || true)"
check "the installed sotd is the old one" "old" "$(cat "$d/prefix/bin/sotd")"
check "no .prev copy was written" "gone" "$([ -e "$d/prefix/bin/sotd.prev" ] && echo exists || echo gone)"

# ---------------------------------------------------------------------------
# pack-build.sh without a musl compiler: a gnu sotd only from a host whose glibc is within the fleet floor. The script
# is copied into a throwaway tree so it cd's there; PATH holds stubs only, so no musl compiler is found. Its first check
# refuses every host but Linux x86_64, so its cases are that platform's: elsewhere they print SKIP lines.
mk_pack_fixture() {  # <dir> <glibc-version>: <dir>/tree/scripts/pack-build.sh, <dir>/bin with stub getconf
    local d="$1"
    mkdir -p "$d/tree/scripts" "$d/bin"
    cp "$SRC/scripts/pack-build.sh" "$d/tree/scripts/"
    printf '#!/bin/sh\necho "glibc %s"\n' "$2" > "$d/bin/getconf"; chmod 0755 "$d/bin/getconf"
    for t in uname dirname sort tail cut; do ln -s "$(command -v "$t")" "$d/bin/$t"; done
}
run_pack() {  # <dir>: sets PK_RC and PK_OUT
    local d="$1"
    PK_RC=0
    PK_OUT="$(env -i PATH="$d/bin" /bin/bash "$d/tree/scripts/pack-build.sh" "$d/out" 2>&1)" || PK_RC=$?
}
pack_check() {  # <description> <expected> <actual>: check on Linux x86_64; elsewhere a SKIP line
    if [ "$(uname -sm)" = "Linux x86_64" ]; then check "$@"; else printf '    SKIP %s (pack-build.sh runs on Linux x86_64 only)\n' "$1"; fi
}
case_start "pack_build_refuses_a_too_new_glibc_without_musl"
d="$WORK/pk-new"; mk_pack_fixture "$d" 2.39; run_pack "$d"
pack_check "the pack exits non-zero" "yes" "$([ "$PK_RC" -ne 0 ] && echo yes || echo no)"
pack_check "it names the glibc floor route" "1" "$(printf '%s\n' "$PK_OUT" | grep -c 'glibc <= 2.31' || true)"
pack_check "it does not ask for a package install" "0" "$(printf '%s\n' "$PK_OUT" | grep -c 'install musl' || true)"
pack_check "nothing was packed" "gone" "$([ -e "$d/out" ] && echo exists || echo gone)"
case_start "pack_build_passes_the_glibc_gate_at_the_floor"
d="$WORK/pk-ok"; mk_pack_fixture "$d" 2.31; run_pack "$d"
pack_check "the glibc gate does not refuse" "0" "$(printf '%s\n' "$PK_OUT" | grep -c 'newer than the fleet' || true)"
pack_check "it goes on to the tree checks, which this git-less PATH stops" "yes" "$(printf '%s\n' "$PK_OUT" | grep -q 'git' && echo yes || echo no)"
case_start "pack_build_names_an_unreadable_glibc_without_musl"
d="$WORK/pk-bad-getconf"; mk_pack_fixture "$d" 2.31; printf '#!/bin/sh\nexit 1\n' > "$d/bin/getconf"; run_pack "$d"
pack_check "the pack exits non-zero when getconf fails" "yes" "$([ "$PK_RC" -ne 0 ] && echo yes || echo no)"
pack_check "it names the unreadable glibc when getconf fails" "1" "$(printf '%s\n' "$PK_OUT" | grep -c 'no readable glibc version' || true)"
d="$WORK/pk-no-getconf"; mk_pack_fixture "$d" 2.31; rm -f "$d/bin/getconf"; run_pack "$d"
pack_check "the pack exits non-zero when getconf is absent" "yes" "$([ "$PK_RC" -ne 0 ] && echo yes || echo no)"
pack_check "it names the unreadable glibc when getconf is absent" "1" "$(printf '%s\n' "$PK_OUT" | grep -c 'no readable glibc version' || true)"
case_start "pack_build_orders_glibc_versions_by_number"
for v in 2.4 2.9; do
    d="$WORK/pk-order-$v"; mk_pack_fixture "$d" "$v"; run_pack "$d"
    pack_check "glibc $v is older than the floor, so the gate does not refuse it" "0" "$(printf '%s\n' "$PK_OUT" | grep -c 'newer than the fleet' || true)"
done
case_start "pack_build_takes_the_musl_branch_when_a_musl_compiler_is_present"
d="$WORK/pk-musl"; mk_pack_fixture "$d" 2.39; printf '#!/bin/sh\nexit 0\n' > "$d/bin/musl-gcc"; chmod 0755 "$d/bin/musl-gcc"; run_pack "$d"
pack_check "with a musl compiler the glibc gate does not refuse 2.39" "0" "$(printf '%s\n' "$PK_OUT" | grep -c 'newer than the fleet' || true)"
pack_check "it goes on to the tree checks, which this git-less PATH stops" "yes" "$(printf '%s\n' "$PK_OUT" | grep -q 'git' && echo yes || echo no)"

# ---------------------------------------------------------------------------
case_start "archive_install_prints_the_libc"
d="$WORK/ar-libc"; mk_packed "$d" 1.2.3-dev 012345678 1.2.3-dev
printf 'gnu (glibc 2.31, runs on glibc >= 2.31)\n' > "$d/arch/LIBC"
( cd "$d/arch" && sha256_of "sot-1.2.3-dev-$TGT.tar.gz" COMMIT LIBC > SHA256SUMS ); run_packed "$d"
check "the install prints the libc line" "1" "$(printf '%s\n' "$AR_OUT" | grep -c 'archive sotd libc: gnu (glibc 2.31' || true)"

# ---------------------------------------------------------------------------
# A sotd that cannot run here: the install prints its loader's error and the libc its pack names, and blames nothing.
case_start "archive_install_names_the_libc_when_its_sotd_cannot_run"
d="$WORK/ar-norun"; mk_packed "$d" 1.2.3-dev 012345678 1.2.3-dev
printf 'gnu (glibc 2.31, runs on glibc >= 2.31)\n' > "$d/arch/LIBC"
printf '#!/bin/sh\necho "loader: GLIBC_2.34 not found (required by sotd)" >&2\nexit 1\n' > "$d/pack/sot-1.2.3-dev-$TGT/sotd"
chmod 0755 "$d/pack/sot-1.2.3-dev-$TGT/sotd"
tar -czf "$d/arch/sot-1.2.3-dev-$TGT.tar.gz" -C "$d/pack" "sot-1.2.3-dev-$TGT"
( cd "$d/arch" && sha256_of "sot-1.2.3-dev-$TGT.tar.gz" COMMIT LIBC > SHA256SUMS ); run_packed "$d"
check "an unrunnable sotd makes the install exit non-zero" "yes" "$([ "$AR_RC" -ne 0 ] && echo yes || echo no)"
check "it prints the loader's error" "1" "$(printf '%s\n' "$AR_OUT" | grep -c 'GLIBC_2.34 not found' || true)"
check "it names the pack's libc" "1" "$(printf '%s\n' "$AR_OUT" | grep -c 'archive sotd libc: gnu (glibc 2.31' || true)"
check "it does not blame TMPDIR" "0" "$(printf '%s\n' "$AR_OUT" | grep -c 'noexec' || true)"
check "the installed sotd is the old one" "old" "$(cat "$d/prefix/bin/sotd")"

# ---------------------------------------------------------------------------
# --ignore-missing checks only the names SHA256SUMS lists, so a copied COMMIT or LIBC it does not list is refused, not trusted.
case_start "archive_install_refuses_a_copied_asset_its_sha256sums_does_not_list"
d="$WORK/ar-unlisted-libc"; mk_packed "$d" 1.2.3-dev 012345678 1.2.3-dev
printf 'gnu (glibc 2.31, runs on glibc >= 2.31)\n' > "$d/arch/LIBC"; run_packed "$d"
check "an unlisted LIBC makes the install exit non-zero" "yes" "$([ "$AR_RC" -ne 0 ] && echo yes || echo no)"
check "it names LIBC as unlisted" "1" "$(printf '%s\n' "$AR_OUT" | grep -c 'LIBC is not listed in' || true)"
check "it refuses before the commit is fetched" "0" "$(printf '%s\n' "$AR_OUT" | grep -c 'is not in origin' || true)"
check "the installed sotd is the old one" "old" "$(cat "$d/prefix/bin/sotd")"
d="$WORK/ar-unlisted-commit"; mk_packed "$d" 1.2.3-dev 012345678 1.2.3-dev
( cd "$d/arch" && sha256_of "sot-1.2.3-dev-$TGT.tar.gz" > SHA256SUMS ); run_packed "$d"
check "an unlisted COMMIT makes the install exit non-zero" "yes" "$([ "$AR_RC" -ne 0 ] && echo yes || echo no)"
check "it names COMMIT as unlisted" "1" "$(printf '%s\n' "$AR_OUT" | grep -c 'COMMIT is not listed in' || true)"
check "it refuses before the commit is fetched" "0" "$(printf '%s\n' "$AR_OUT" | grep -c 'is not in origin' || true)"
check "the installed sotd is the old one" "old" "$(cat "$d/prefix/bin/sotd")"

# ---------------------------------------------------------------------------
printf '\n'
if [ "$fails" -eq 0 ]; then
    printf 'installer-apply: all checks passed\n'
else
    printf 'installer-apply: %d check(s) FAILED\n' "$fails" >&2
    exit 1
fi
