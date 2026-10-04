#!/usr/bin/env bash
# installer-apply.sh -- the sot-apply transaction, the one-copy helper and the network refusal.
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
    local stage="$1/prefix/updates/v9.9.9-linux-x86_64" top=sot-v9.9.9-linux-x86_64 sha unit_owner wrap_owner
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
    sha="$(sha256sum "$stage/$top.tar.gz" | cut -d' ' -f1)"
    printf '{}\n' > "$stage/manifest.json"
    printf '{\n  "tag": "v9.9.9",\n  "target": "linux-x86_64",\n  "checkout": "%s",\n  "commit": "%s",\n  "asset": "%s.tar.gz",\n  "asset_sha256": "%s"\n}\n' \
        "$co" "$(cd "$co" && git rev-parse HEAD)" "$top" "$sha" > "$P/updates/pending-linux-x86_64.json"
    unit_owner="$other"; [ "$ou" = 1 ] && unit_owner="$P"
    wrap_owner="$other"; [ "$ow" = 1 ] && wrap_owner="$P"
    printf '[Service]\nExecStart=%s/bin/sotd --x\nRestart=always\n' "$unit_owner" > "$h/.config/systemd/user/sotd.service"
    printf '#!/usr/bin/env bash\nPENDING="%s/updates/pending-linux-x86_64.json"\nstart_daemon_if_needed() {\n:\n}\n' "$wrap_owner" > "$h/.local/bin/sot-launch"
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
check "exactly one daemon-reload" "1" "$(reloads "$d")"
check "the unit backup equals the original" "same" "$(cmp -s "$d/unit.orig" "$d/prefix/updates/sotd.service.prev-linux-x86_64" && echo same || echo differ)"

# ---------------------------------------------------------------------------
case_start "apply_skips_foreign_unit"
d="$WORK/ap2"; mk_apply_fixture "$d" 0 0; run_apply "$d"
check "the applied tag is recorded" "1" "$(grep -c '"tag": "v9.9.9"' "$d/prefix/install.json" || true)"
check "a foreign unit is byte-identical" "same" "$(cmp -s "$d/unit.orig" "$d/home/.config/systemd/user/sotd.service" && echo same || echo differ)"
check "no daemon-reload" "0" "$(reloads "$d")"
check "no unit backup" "no" "$([ -e "$d/prefix/updates/sotd.service.prev-linux-x86_64" ] && echo yes || echo no)"

# ---------------------------------------------------------------------------
case_start "apply_rerenders_owned_wrapper"
d="$WORK/ap3"; mk_apply_fixture "$d" 1 1
W="$d/home/.local/bin/sot-launch"; ino0="$(stat -c %i "$W")"
run_apply "$d"
check "the wrapper carries the marker" "1" "$(grep -c '^# sot-launch: all-in-one$' "$W" || true)"
check "the wrapper is executable" "yes" "$([ -x "$W" ] && echo yes || echo no)"
check "the wrapper is a new inode" "changed" "$([ "$(stat -c %i "$W")" != "$ino0" ] && echo changed || echo same)"
check "the wrapper backup equals the original" "same" "$(cmp -s "$d/wrap.orig" "$d/prefix/updates/sot-launch.prev-linux-x86_64" && echo same || echo differ)"

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
check "two daemon-reloads (re-render, restore)" "2" "$(reloads "$d")"
check "sotd is back to its pre-apply content" "old-sotd" "$(cat "$d/prefix/bin/sotd")"
check "repo/current points at the previous checkout" "$d/prev-co" "$(readlink "$d/prefix/repo/current")"
check "the pending pointer is still armed" "yes" "$([ -f "$d/prefix/updates/pending-linux-x86_64.json" ] && echo yes || echo no)"
check "the wrapper is byte-equal to the original" "same" "$(cmp -s "$d/wrap.orig" "$d/home/.local/bin/sot-launch" && echo same || echo differ)"

# ---------------------------------------------------------------------------
case_start "rollback_restores_unit_and_wrapper"
d="$WORK/ap6"; mk_apply_fixture "$d" 1 1; run_apply "$d"
check "the apply re-rendered the unit" "1" "$(grep -c '^Restart=on-failure$' "$d/home/.config/systemd/user/sotd.service" || true)"
run_apply "$d" --rollback
check "the unit is byte-equal to the original" "same" "$(cmp -s "$d/unit.orig" "$d/home/.config/systemd/user/sotd.service" && echo same || echo differ)"
check "the wrapper is byte-equal to the original" "same" "$(cmp -s "$d/wrap.orig" "$d/home/.local/bin/sot-launch" && echo same || echo differ)"
check "the wrapper is executable" "yes" "$([ -x "$d/home/.local/bin/sot-launch" ] && echo yes || echo no)"
check "a daemon-reload followed the restore" "2" "$(reloads "$d")"
check "the checkout flipped back" "$d/prev-co" "$(readlink "$d/prefix/repo/current")"

# ---------------------------------------------------------------------------
case_start "failed_apply_keeps_old_record_and_pending"
d="$WORK/ap7"; mk_apply_fixture "$d" 1 1
chmod 0555 "$d/home/.local/bin"
run_apply "$d"
chmod 0755 "$d/home/.local/bin"
check "install.json still names the old tag" "1" "$(grep -c '"tag": "v9.9.8"' "$d/prefix/install.json" || true)"
check "install.json still names the old commit" "1" "$(grep -c '"commit": "aaa"' "$d/prefix/install.json" || true)"
check "the pending pointer is still armed" "yes" "$([ -f "$d/prefix/updates/pending-linux-x86_64.json" ] && echo yes || echo no)"
run_apply "$d"
check "a second apply installs the new tag" "1 new-sotd no" \
    "$(grep -c '"tag": "v9.9.9"' "$d/prefix/install.json" || true) $(cat "$d/prefix/bin/sotd") $([ -f "$d/prefix/updates/pending-linux-x86_64.json" ] && echo yes || echo no)"

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
check "the pending pointer is kept" "yes" "$([ -f "$d/prefix/updates/pending-linux-x86_64.json" ] && echo yes || echo no)"
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
printf 'older-unit\n' > "$d/prefix/updates/sotd.service.prev-linux-x86_64"
printf 'older-wrap\n' > "$d/prefix/updates/sot-launch.prev-linux-x86_64"
cp "$d/prefix/bin/sot-apply" "$d/apply.orig"
half_cp_stub "$d" '*/bin/sotd.prev*'
run_apply "$d"
check "the apply exits non-zero" "yes" "$([ "$AP_RC" -ne 0 ] && echo yes || echo no)"
check "the failed backup is named" "1" "$(grep -c 'backing up sotd failed' "$d/out" || true)"
check "no installed file changed" "old-sot old-sotd same same same" \
    "$(cat "$d/prefix/bin/sot") $(cat "$d/prefix/bin/sotd") $(cmp -s "$d/apply.orig" "$d/prefix/bin/sot-apply" && echo same || echo differ) $(cmp -s "$d/unit.orig" "$d/home/.config/systemd/user/sotd.service" && echo same || echo differ) $(cmp -s "$d/wrap.orig" "$d/home/.local/bin/sot-launch" && echo same || echo differ)"
check "the old backup set is intact" "older-sot older-sotd older-unit older-wrap" \
    "$(cat "$d/prefix/bin/sot.prev") $(cat "$d/prefix/bin/sotd.prev") $(cat "$d/prefix/updates/sotd.service.prev-linux-x86_64") $(cat "$d/prefix/updates/sot-launch.prev-linux-x86_64")"
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
sed -i 's/exit 22 ;; esac/exit 6 ;; esac/' "$d/stubs/curl"
rc=0
out="$(env -i HOME="$d/home" PATH="$d/stubs:/usr/bin:/bin" CURL_LOG="$d/curl.log" SOT_INSTALL_TAG=v0.0.1 \
    bash "$(dirname "$0")/../install.sh" --local --no-service --prefix "$d/prefix" 2>&1)" || rc=$?
check "an unreachable network exits non-zero" "yes" "$([ "$rc" -ne 0 ] && echo yes || echo no)"
check "it names the network, not the release" "1 0" \
    "$(printf '%s\n' "$out" | grep -c 'cannot reach raw.githubusercontent.com to check release v0.0.1 (curl exit 6)' || true) $(printf '%s\n' "$out" | grep -c 'predates' || true)"
check "the prefix was still never created" "gone" "$([ -e "$d/prefix" ] && echo exists || echo gone)"

# ---------------------------------------------------------------------------
printf '\n'
if [ "$fails" -eq 0 ]; then
    printf 'installer-apply: all checks passed\n'
else
    printf 'installer-apply: %d check(s) FAILED\n' "$fails" >&2
    exit 1
fi
